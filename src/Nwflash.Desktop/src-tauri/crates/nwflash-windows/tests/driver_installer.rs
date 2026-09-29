use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use nwflash_windows::{
    locate_bundled_driver_archive, write_vivo_adb_usb_ids, DriverInstaller,
    ElevatedProcessExecutor, ProcessCommand, ProcessOutput,
};

fn temporary_directory(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be available")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("nwflash-{label}-{nonce}"));
    fs::create_dir_all(&path).expect("temporary directory should be created");
    path
}

fn fixture_archive(root: &Path) -> PathBuf {
    let archive = root.join("fixture-driver.7z");
    let bundled = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("resources")
        .join("drivers")
        .join("vivo-usb-driver.7z");
    fs::copy(bundled, &archive).expect("trusted fixture archive should be copied");
    archive
}

#[test]
fn adb_usb_ini_adds_each_vivo_id_once() {
    let root = temporary_directory("adb-usb-ini");
    let ini = root.join(".android").join("adb_usb.ini");
    fs::create_dir_all(ini.parent().expect("ini parent should exist"))
        .expect("ini parent should be created");
    fs::write(&ini, "0x2D95\n0x2d95\ncomment\n").expect("initial ini should be written");

    write_vivo_adb_usb_ids(&ini).expect("vivo ids should be written");

    let lines = fs::read_to_string(&ini).expect("ini should be readable");
    assert_eq!(
        lines
            .lines()
            .filter(|line| line.eq_ignore_ascii_case("0x2D95"))
            .count(),
        2
    );
    assert!(lines.lines().any(|line| line == "0x9BB5"));
    assert!(lines.lines().any(|line| line == "0x18D1"));
    assert!(lines.lines().any(|line| line == "0x0E8D"));
    fs::remove_dir_all(root).expect("temporary directory should be removed");
}

#[test]
fn bundled_driver_archive_is_resolved_only_from_the_fixed_application_resource_path() {
    let root = temporary_directory("driver-bundle-location");
    let drivers = root.join("drivers");
    fs::create_dir_all(&drivers).expect("drivers directory should be created");
    let expected = drivers.join("vivo-usb-driver.7z");
    fs::write(&expected, "bundle").expect("bundle fixture should be written");

    assert_eq!(locate_bundled_driver_archive(&root), Some(expected));
    fs::remove_file(root.join("drivers").join("vivo-usb-driver.7z"))
        .expect("bundle fixture should be removed");
    assert_eq!(locate_bundled_driver_archive(&root), None);
    fs::remove_dir_all(root).expect("temporary directory should be removed");
}

#[test]
fn bundled_driver_digest_is_compiled_in_and_matches_release_manifest_and_resource() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repository_root = crate_root
        .ancestors()
        .nth(5)
        .expect("repository root should be reachable");
    let archive = crate_root
        .join("..")
        .join("..")
        .join("resources")
        .join("drivers")
        .join("vivo-usb-driver.7z");
    let release_manifest =
        fs::read_to_string(repository_root.join("packaging/release/tauri-resources.json"))
            .expect("release resource manifest should be readable");
    let source_marker =
        "\"source\": \"src/Nwflash.Desktop/src-tauri/resources/drivers/vivo-usb-driver.7z\"";
    let source_index = release_manifest
        .find(source_marker)
        .expect("release manifest must contain the driver archive");
    let entry_start = release_manifest[..source_index]
        .rfind('{')
        .expect("driver manifest entry must start with an object");
    let entry_end = release_manifest[source_index..]
        .find('}')
        .map(|offset| source_index + offset + 1)
        .expect("driver manifest entry must end with an object");
    let driver_entry = &release_manifest[entry_start..entry_end];

    let root = temporary_directory("driver-shipped-resource");
    let executor = RecordingElevatedExecutor::with_exit_code(0);
    let installer = DriverInstaller::with_dependencies(
        archive.clone(),
        root.join("staging"),
        root.join(".android").join("adb_usb.ini"),
        executor.clone(),
    );
    assert_eq!(
        installer
            .install()
            .expect("shipped archive must match the compiled digest"),
        0
    );
    let command = executor
        .command()
        .expect("only the verified archive may supply an INF to pnputil");
    assert_eq!(command.args[0], "/add-driver");
    // 单条 pnputil 安装全部 INF(只提权一次)：通配符 + `/subdirs` 递归。
    // 逐条喂单个 INF 会在设备绑定阶段以 CONFIGRET 0xE000024B 失败，
    // 所以这里必须是通配符形态。通配符根是解包目录(全部 INF 的公共祖先)，
    // 它位于本次运行的 staging 目录内。
    assert_eq!(command.args[2], "/subdirs");
    assert_eq!(command.args.last(), Some(&"/install".to_string()));
    assert_eq!(command.args.len(), 4);
    let pattern = &command.args[1];
    assert!(
        pattern.ends_with(r"\extracted\*.inf"),
        "unexpected pattern: {pattern}"
    );
    assert!(
        pattern.starts_with(&root.join("staging").to_string_lossy().to_string()),
        "pattern must stay inside this run's staging root: {pattern}"
    );
    assert_eq!(
        fs::metadata(&archive)
            .expect("shipped archive metadata should be readable")
            .len(),
        12_199_572
    );
    assert!(driver_entry.contains(
        "\"sha256\": \"22fa20b21004a7ae76668716ef51e22fd9e8e9eeea226a035ad23157441b60ea\""
    ));
    assert!(driver_entry.contains(source_marker));
    fs::remove_dir_all(root).expect("temporary directory should be removed");
}

#[test]
fn replaced_driver_archive_fails_before_extraction_or_elevation() {
    let root = temporary_directory("driver-integrity-replaced");
    let archive = root.join("vivo-usb-driver.7z");
    fs::write(&archive, b"replacement with a fake valid INF layout")
        .expect("replacement fixture should be written");
    let executor = RecordingElevatedExecutor::with_exit_code(0);
    let installer = DriverInstaller::with_dependencies(
        archive,
        root.join("staging"),
        root.join(".android").join("adb_usb.ini"),
        executor.clone(),
    );

    let error = installer
        .install()
        .expect_err("a replaced archive must fail closed");

    assert!(error.to_string().contains("完整性"));
    assert!(executor.command().is_none(), "UAC/elevation must not start");
    fs::remove_dir_all(root).expect("temporary directory should be removed");
}

#[cfg(windows)]
#[test]
fn forged_windir_cannot_redirect_pnputil_to_an_attacker_directory() {
    let root = temporary_directory("driver-forged-windir");
    let attacker_windows = root.join("attacker-windows");
    fs::create_dir_all(attacker_windows.join("System32")).expect("attacker directory exists");
    fs::write(
        attacker_windows.join("System32").join("pnputil.exe"),
        b"attacker",
    )
    .expect("attacker executable should be written");
    let previous = std::env::var_os("WINDIR");
    std::env::set_var("WINDIR", &attacker_windows);

    let executor = RecordingElevatedExecutor::with_exit_code(0);
    let installer = DriverInstaller::with_dependencies(
        fixture_archive(&root),
        root.join("staging"),
        root.join(".android").join("adb_usb.ini"),
        executor.clone(),
    );
    let result = installer.install();

    match previous {
        Some(value) => std::env::set_var("WINDIR", value),
        None => std::env::remove_var("WINDIR"),
    }
    let command = executor
        .command()
        .expect("verified archive should reach the elevated executor");
    let attacker_windows = attacker_windows.to_string_lossy().into_owned();
    assert!(result.is_ok());
    assert!(
        !command.program.starts_with(&attacker_windows),
        "pnputil must come from the OS system directory, never WINDIR"
    );
    assert!(command
        .program
        .to_ascii_lowercase()
        .ends_with("\\system32\\pnputil.exe"));
    fs::remove_dir_all(root).expect("temporary directory should be removed");
}

#[cfg(windows)]
#[test]
fn verified_driver_tree_stays_locked_and_uses_exact_inf_during_elevation_window() {
    let root = temporary_directory("driver-elevation-locks");
    let executor = LockCheckingElevatedExecutor::default();
    let installer = DriverInstaller::with_dependencies(
        fixture_archive(&root),
        root.join("staging"),
        root.join(".android").join("adb_usb.ini"),
        executor.clone(),
    );

    assert_eq!(
        installer.install().expect("locked install should succeed"),
        0
    );
    let results = executor.results();
    assert!(!results.is_empty());
    assert!(results
        .iter()
        .all(|result| result.inf_cat_sys_write_blocked));
    assert!(results
        .iter()
        .all(|result| result.inf_cat_sys_delete_blocked));
    assert!(results
        .iter()
        .all(|result| result.inf_cat_sys_rename_blocked));
    assert!(results
        .iter()
        .all(|result| result.inf_cat_sys_replace_blocked));
    assert!(results.iter().all(|result| result.parent_rename_blocked));
    assert!(results.iter().all(|result| {
        // 通配符形态：模式由解包根拼出，绝不会把注入的 malicious.inf 显式列进去。
        let pattern = &result.command.args[1];
        pattern.ends_with(r"\extracted\*.inf") && !pattern.contains("malicious")
    }));
    fs::remove_dir_all(root).expect("temporary directory should be removed");
}

#[cfg(windows)]
#[test]
fn inf_injected_during_extraction_is_rejected_before_elevation() {
    let root = temporary_directory("driver-extraction-injection");
    let staging_root = root.join("staging");
    let executor = RecordingElevatedExecutor::with_exit_code(0);
    let installer = DriverInstaller::with_dependencies(
        fixture_archive(&root),
        staging_root.clone(),
        root.join(".android").join("adb_usb.ini"),
        executor.clone(),
    );
    let attacker = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if let Ok(entries) = fs::read_dir(&staging_root) {
                for entry in entries.flatten() {
                    let extracted = entry.path().join("extracted");
                    if extracted.is_dir()
                        && fs::OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .open(extracted.join("malicious.inf"))
                            .is_ok()
                    {
                        return true;
                    }
                }
            }
            std::thread::yield_now();
        }
        false
    });

    let result = installer.install();
    assert!(
        attacker.join().expect("attacker thread should finish"),
        "attack fixture must land during extraction"
    );
    assert!(result.is_err(), "unexpected archive entry must fail closed");
    assert!(executor.command().is_none(), "elevation must not start");
    fs::remove_dir_all(root).expect("temporary directory should be removed");
}

#[cfg(windows)]
#[test]
fn reparse_staging_root_fails_before_extraction_or_elevation() {
    let root = temporary_directory("driver-reparse-staging");
    let real_staging = root.join("real-staging");
    let reparse_staging = root.join("staging-junction");
    fs::create_dir(&real_staging).expect("real staging should be created");
    let status = std::process::Command::new("cmd")
        .arg("/D")
        .arg("/C")
        .arg("mklink")
        .arg("/J")
        .arg(&reparse_staging)
        .arg(&real_staging)
        .status()
        .expect("junction command should run");
    assert!(status.success(), "junction fixture should be created");
    let executor = RecordingElevatedExecutor::with_exit_code(0);
    let installer = DriverInstaller::with_dependencies(
        fixture_archive(&root),
        reparse_staging.clone(),
        root.join(".android").join("adb_usb.ini"),
        executor.clone(),
    );

    let error = installer
        .install()
        .expect_err("reparse staging must fail closed");

    assert!(error.to_string().contains("完整性"));
    assert!(executor.command().is_none());
    fs::remove_dir(&reparse_staging).expect("junction should be removed without following it");
    fs::remove_dir_all(root).expect("temporary directory should be removed");
}

#[test]
fn driver_installer_runs_elevated_pnputil_then_writes_adb_ids_and_cleans_staging() {
    let root = temporary_directory("driver-install-success");
    let staging_root = root.join("staging");
    let adb_ini = root.join(".android").join("adb_usb.ini");
    let executor = RecordingElevatedExecutor::with_exit_code(0);
    let installer = DriverInstaller::with_dependencies(
        fixture_archive(&root),
        staging_root.clone(),
        adb_ini.clone(),
        executor.clone(),
    );

    assert_eq!(
        installer.install().expect("driver install should succeed"),
        0
    );

    let command = executor
        .command()
        .expect("pnputil command should be captured");
    assert!(command.program.ends_with("pnputil.exe"));
    assert_eq!(command.args[0], "/add-driver");
    // 一条通配符命令递归装完整棵树，尾部固定 `/install`。
    assert!(
        command.args[1].ends_with(".inf"),
        "wildcard must target .inf: {}",
        command.args[1]
    );
    assert_eq!(command.args[2], "/subdirs");
    assert_eq!(command.args.last(), Some(&"/install".to_string()));
    let adb_ids = fs::read_to_string(adb_ini).expect("adb ids should be written after success");
    assert!(adb_ids.contains("0x2D95"));
    assert!(adb_ids.contains("0x9BB5"));
    assert!(
        fs::read_dir(staging_root)
            .expect("staging root should remain readable")
            .next()
            .is_none(),
        "per-install staging directory must be removed after install"
    );
    fs::remove_dir_all(root).expect("temporary directory should be removed");
}

#[test]
fn driver_installer_treats_already_present_as_success() {
    // pnputil 退出码 5 =「INF 处理成功但无新增（已存在）」。重装已装好的
    // 驱动必须算成功：整体退出码归一为 0，adb_usb.ini 照常补写——此前
    // `== 0` 判定与旧的 `!= 0` 判定对 5 的结论相同，重装必报失败。
    let root = temporary_directory("driver-install-already-present");
    let executor = RecordingElevatedExecutor::with_exit_code(5);
    let adb_ini = root.join(".android").join("adb_usb.ini");
    let installer = DriverInstaller::with_dependencies(
        fixture_archive(&root),
        root.join("staging"),
        adb_ini.clone(),
        executor,
    );

    assert_eq!(
        installer
            .install()
            .expect("already-present install should succeed"),
        0
    );
    let adb_ids = fs::read_to_string(adb_ini).expect("adb ids should be written after success");
    assert!(adb_ids.contains("0x2D95"));
    fs::remove_dir_all(root).expect("temporary directory should be removed");
}

#[test]
fn driver_installer_skips_adb_ids_when_pnputil_fails() {
    // 真实失败码（1 = 用法错误 / INF 被拒）才算失败：不写 adb_usb.ini。
    let root = temporary_directory("driver-install-failure");
    let executor = RecordingElevatedExecutor::with_exit_code(1);
    let adb_ini = root.join(".android").join("adb_usb.ini");
    let installer = DriverInstaller::with_dependencies(
        fixture_archive(&root),
        root.join("staging"),
        adb_ini.clone(),
        executor,
    );

    assert_eq!(
        installer
            .install()
            .expect("nonzero exit should be returned"),
        1
    );
    assert!(
        !adb_ini.exists(),
        "failed installation must not write adb_usb.ini"
    );
    fs::remove_dir_all(root).expect("temporary directory should be removed");
}

#[test]
fn driver_installer_rejects_output_reported_failure_even_when_exit_code_is_5() {
    // 2026-09-28 实测：某条 INF 报 `Failed to add driver package: Access is denied`
    // 时，pnputil 整体退出码**仍然是 5**（提权下为 0）。只按退出码判定会把这个
    // 真实失败报成成功。这里钉死：输出里有失败行就算失败，不写 adb_usb.ini。
    let root = temporary_directory("driver-install-hidden-failure");
    let adb_ini = root.join(".android").join("adb_usb.ini");
    let executor = RecordingElevatedExecutor::with_exit_code_and_output(
        5,
        "Adding driver package:  android_usb.inf\nFailed to add driver package: Access is denied.\n\nTotal driver packages:  1\nAdded driver packages:  0\n",
    );
    let installer = DriverInstaller::with_dependencies(
        fixture_archive(&root),
        root.join("staging"),
        adb_ini.clone(),
        executor,
    );

    let outcome = installer
        .install_with_cancel_detailed(|| false)
        .expect("install should return an outcome");
    assert!(
        outcome.reported_failure,
        "output-reported failure must be surfaced"
    );
    assert!(
        !nwflash_windows::driver_install_succeeded(&outcome),
        "exit code 5 with a Failed line must not count as success"
    );
    assert_eq!(outcome.added_packages, Some(0));
    assert!(
        !adb_ini.exists(),
        "failed installation must not write adb_usb.ini"
    );
    fs::remove_dir_all(root).expect("temporary directory should be removed");
}

#[test]
fn driver_installer_reports_added_package_count_from_pnputil_summary() {
    // `Added driver packages:` 是比退出码可靠的计数：逐条形态下 8 次调用
    // 全部返回 5，只有汇总行反映真实结果。
    let root = temporary_directory("driver-install-added-count");
    let executor = RecordingElevatedExecutor::with_exit_code_and_output(
        0,
        "Adding driver package:  android_winusb.inf\nDriver package added successfully.\n\nTotal driver packages:  8\nAdded driver packages:  7\n",
    );
    let installer = DriverInstaller::with_dependencies(
        fixture_archive(&root),
        root.join("staging"),
        root.join(".android").join("adb_usb.ini"),
        executor,
    );

    let outcome = installer
        .install_with_cancel_detailed(|| false)
        .expect("install should return an outcome");
    assert!(nwflash_windows::driver_install_succeeded(&outcome));
    assert_eq!(outcome.added_packages, Some(7));
    assert!(!outcome.reported_failure);
    fs::remove_dir_all(root).expect("temporary directory should be removed");
}

#[test]
fn driver_installer_cancels_before_elevated_execution_and_cleans_staging() {
    let root = temporary_directory("driver-install-cancel");
    let executor = RecordingElevatedExecutor::with_exit_code(0);
    let staging_root = root.join("staging");
    let installer = DriverInstaller::with_dependencies(
        fixture_archive(&root),
        staging_root.clone(),
        root.join(".android").join("adb_usb.ini"),
        executor.clone(),
    );

    let error = installer
        .install_with_cancel(|| true)
        .expect_err("cancelled installation must not launch pnputil");

    assert!(error.to_string().contains("用户取消"));
    assert!(
        executor.command().is_none(),
        "pnputil must not run after cancellation"
    );
    assert!(
        fs::read_dir(staging_root)
            .expect("staging root should remain readable")
            .next()
            .is_none(),
        "per-install staging directory must be removed after cancellation"
    );
    fs::remove_dir_all(root).expect("temporary directory should be removed");
}

#[cfg(windows)]
#[derive(Debug, Clone)]
struct LockCheckResult {
    command: ProcessCommand,
    inf_cat_sys_write_blocked: bool,
    inf_cat_sys_delete_blocked: bool,
    inf_cat_sys_rename_blocked: bool,
    inf_cat_sys_replace_blocked: bool,
    parent_rename_blocked: bool,
}

#[cfg(windows)]
#[derive(Clone, Default)]
struct LockCheckingElevatedExecutor {
    results: Arc<Mutex<Vec<LockCheckResult>>>,
}

#[cfg(windows)]
impl LockCheckingElevatedExecutor {
    fn results(&self) -> Vec<LockCheckResult> {
        self.results
            .lock()
            .expect("lock result should not be poisoned")
            .clone()
    }
}

#[cfg(windows)]
impl ElevatedProcessExecutor for LockCheckingElevatedExecutor {
    fn run_elevated(
        &self,
        command: ProcessCommand,
    ) -> Result<ProcessOutput, nwflash_domain::DomainError> {
        fn collect_sensitive_files(
            directory: &Path,
            files: &mut std::collections::BTreeMap<String, PathBuf>,
        ) {
            for entry in
                fs::read_dir(directory).expect("frozen driver directory should be readable")
            {
                let path = entry.expect("driver entry should be readable").path();
                if path.is_dir() {
                    collect_sensitive_files(&path, files);
                } else if path.extension().is_some_and(|extension| {
                    ["inf", "cat", "sys"]
                        .iter()
                        .any(|expected| extension.eq_ignore_ascii_case(expected))
                }) {
                    let extension = path
                        .extension()
                        .expect("sensitive file must have an extension")
                        .to_string_lossy()
                        .to_ascii_lowercase();
                    files.entry(extension).or_insert(path);
                }
            }
        }

        // 通配符形态：args[1] 是 `<extracted>\*.inf`，取它的父目录即解包根。
        let pattern = PathBuf::from(&command.args[1]);
        let extracted = pattern
            .parent()
            .expect("wildcard pattern should have a parent")
            .to_path_buf();
        assert!(
            extracted
                .file_name()
                .is_some_and(|name| name == "extracted"),
            "wildcard root must be the extracted directory: {pattern:?}"
        );
        let mut files = std::collections::BTreeMap::new();
        collect_sensitive_files(&extracted, &mut files);
        let files = ["inf", "cat", "sys"]
            .iter()
            .map(|extension| {
                files
                    .get(*extension)
                    .cloned()
                    .unwrap_or_else(|| panic!("fixture must exercise {extension} locking"))
            })
            .collect::<Vec<_>>();
        // 承载这些受保护文件的子目录（INF 所在的驱动子目录）也必须锁住改名。
        let inf_parent = files
            .iter()
            .find(|path| {
                path.extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("inf"))
            })
            .and_then(|path| path.parent())
            .expect("fixture INF must have a parent directory")
            .to_path_buf();
        let write_blocked = |path: &Path| fs::OpenOptions::new().write(true).open(path).is_err();
        let replace_blocked = |path: &Path| {
            let replacement = path.with_extension("replacement");
            fs::write(&replacement, "replacement").is_err()
                || fs::rename(&replacement, path).is_err()
        };
        let result = LockCheckResult {
            command,
            inf_cat_sys_write_blocked: files.iter().all(|path| write_blocked(path)),
            inf_cat_sys_delete_blocked: files.iter().all(|path| fs::remove_file(path).is_err()),
            inf_cat_sys_rename_blocked: files
                .iter()
                .all(|path| fs::rename(path, path.with_extension("swapped")).is_err()),
            inf_cat_sys_replace_blocked: files.iter().all(|path| replace_blocked(path)),
            parent_rename_blocked: fs::rename(&inf_parent, inf_parent.with_extension("swapped"))
                .is_err(),
        };
        self.results
            .lock()
            .expect("lock result should not be poisoned")
            .push(result);
        Ok(ProcessOutput {
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
        })
    }
}

#[derive(Clone)]
struct RecordingElevatedExecutor {
    exit_code: i32,
    stdout: String,
    command: Arc<Mutex<Option<ProcessCommand>>>,
}

impl RecordingElevatedExecutor {
    fn with_exit_code(exit_code: i32) -> Self {
        Self {
            exit_code,
            stdout: String::new(),
            command: Arc::new(Mutex::new(None)),
        }
    }

    /// 同时带 stdout 的构造器：用于验证「退出码说成功、输出里却有失败行」的场景。
    fn with_exit_code_and_output(exit_code: i32, stdout: &str) -> Self {
        Self {
            exit_code,
            stdout: stdout.to_string(),
            command: Arc::new(Mutex::new(None)),
        }
    }

    fn command(&self) -> Option<ProcessCommand> {
        self.command
            .lock()
            .expect("command lock should not be poisoned")
            .clone()
    }
}

impl ElevatedProcessExecutor for RecordingElevatedExecutor {
    fn run_elevated(
        &self,
        command: ProcessCommand,
    ) -> Result<ProcessOutput, nwflash_domain::DomainError> {
        *self
            .command
            .lock()
            .expect("command lock should not be poisoned") = Some(command);
        Ok(ProcessOutput {
            exit_code: self.exit_code,
            stdout: self.stdout.clone(),
            stderr: String::new(),
        })
    }
}

/// 记录**每一批**命令，用于校验提权批次的形状。
#[derive(Clone, Default)]
struct BatchRecordingExecutor {
    batches: Arc<Mutex<Vec<Vec<ProcessCommand>>>>,
}

impl BatchRecordingExecutor {
    fn batches(&self) -> Vec<Vec<ProcessCommand>> {
        self.batches
            .lock()
            .expect("batch lock should not be poisoned")
            .clone()
    }
}

impl ElevatedProcessExecutor for BatchRecordingExecutor {
    fn run_elevated(
        &self,
        command: ProcessCommand,
    ) -> Result<ProcessOutput, nwflash_domain::DomainError> {
        self.batches
            .lock()
            .expect("batch lock should not be poisoned")
            .push(vec![command]);
        Ok(ProcessOutput {
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
        })
    }

    fn run_elevated_batch(
        &self,
        commands: &[ProcessCommand],
    ) -> Result<Vec<ProcessOutput>, nwflash_domain::DomainError> {
        self.batches
            .lock()
            .expect("batch lock should not be poisoned")
            .push(commands.to_vec());
        Ok(commands
            .iter()
            .map(|_| ProcessOutput {
                exit_code: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
            .collect())
    }
}

/// 安装必须是**一条**通配符 + `/subdirs` 命令，与 C# 版（实测能装成功的那版）
/// 形态一致。
///
/// 2026-09-28 实测推翻了旧的「一条命令一个 INF」结论：
/// * 逐条喂**单个 INF 绝对路径**会在设备绑定阶段以 CONFIGRET `0xE000024B`
///   失败——单独喂一个 INF 时 pnputil 找不到配套 `.cat` catalog。
/// * 一条命令塞**多个显式 INF 路径**才会被整行拒绝（用法错误、退出码 1）。
/// * 只有通配符 `/subdirs` 能把 staging 当一棵驱动包树、连同 catalog 一起解析。
#[test]
fn pnputil_gets_one_wildcard_subdirs_command_in_a_single_elevation() {
    let root = temporary_directory("driver-wildcard-subdirs");
    let executor = BatchRecordingExecutor::default();
    let installer = DriverInstaller::with_dependencies(
        fixture_archive(&root),
        root.join("staging"),
        root.join(".android").join("adb_usb.ini"),
        executor.clone(),
    );

    assert_eq!(
        installer.install().expect("driver install should succeed"),
        0
    );

    let batches = executor.batches();
    assert_eq!(batches.len(), 1, "安装必须在同一次提权里完成");
    let commands = &batches[0];
    assert_eq!(commands.len(), 1, "必须是单条通配符命令，不能拆成逐条 INF");

    let command = &commands[0];
    assert!(command.program.ends_with("pnputil.exe"));
    assert_eq!(command.args[0], "/add-driver");
    assert_eq!(command.args[2], "/subdirs");
    assert_eq!(command.args.last(), Some(&"/install".to_string()));
    assert_eq!(command.args.len(), 4);

    let pattern = &command.args[1];
    assert!(
        pattern.ends_with(r"\*.inf"),
        "通配符必须落在 INF 上：{pattern}"
    );
    // `\\?\` verbatim 前缀会被 pnputil 拒绝（报「系统找不到指定的路径」）。
    assert!(
        !pattern.starts_with("\\\\?\\"),
        "不许把 canonicalize 的 verbatim 前缀交给 pnputil：{pattern}"
    );
    // 通配符根只能落在本次运行的解包目录内，不能指向别处。
    assert!(
        pattern.contains(r"\extracted\"),
        "通配符根必须是本次解包目录：{pattern}"
    );
    fs::remove_dir_all(root).expect("temporary directory should be removed");
}
