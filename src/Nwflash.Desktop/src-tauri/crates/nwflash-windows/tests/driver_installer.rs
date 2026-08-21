use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use nwflash_windows::{
    build_pnputil_install_command, extract_driver_archive, locate_bundled_driver_archive,
    write_vivo_adb_usb_ids, DriverArchiveExtractor, DriverInstaller, ElevatedProcessExecutor,
    ProcessCommand, ProcessOutput,
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

#[test]
fn pnputil_install_command_uses_the_wpf_wildcard_recursive_arguments() {
    let staging = PathBuf::from(r"C:\staging");
    let command = build_pnputil_install_command(&staging).expect("command should build");

    assert!(command.program.ends_with("pnputil.exe"));
    assert_eq!(
        command.args,
        vec![
            "/add-driver".to_string(),
            r"C:\staging\*.inf".to_string(),
            "/subdirs".to_string(),
            "/install".to_string(),
        ]
    );
    assert!(!command.args.iter().any(|argument| argument == "/quiet"));
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
fn bundled_driver_archive_extracts_inf_files_before_pnputil_can_run() {
    let root = temporary_directory("driver-archive");
    let archive = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..")
        .join("..");
    let archive = archive
        .join("VivoKsu.App")
        .join("drivers")
        .join("vivo-usb-driver.7z");

    extract_driver_archive(&archive, &root).expect("bundled driver archive should extract safely");

    assert!(root.exists());
    assert!(std::fs::read_dir(&root)
        .expect("extraction directory should be readable")
        .flatten()
        .any(|entry| entry.path().is_dir()
            || entry
                .path()
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("inf"))));
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
fn driver_installer_runs_elevated_pnputil_then_writes_adb_ids_and_cleans_staging() {
    let root = temporary_directory("driver-install-success");
    let staging_root = root.join("staging");
    let adb_ini = root.join(".android").join("adb_usb.ini");
    let extractor = FixtureExtractor::with_inf();
    let executor = RecordingElevatedExecutor::with_exit_code(0);
    let installer = DriverInstaller::with_dependencies(
        PathBuf::from("bundle.7z"),
        staging_root,
        adb_ini.clone(),
        extractor.clone(),
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
    assert!(command.args[1].ends_with("\\*.inf"));
    assert_eq!(command.args[2..], ["/subdirs", "/install"]);
    let adb_ids = fs::read_to_string(adb_ini).expect("adb ids should be written after success");
    assert!(adb_ids.contains("0x2D95"));
    assert!(adb_ids.contains("0x9BB5"));
    let staging = extractor
        .staging()
        .expect("staging directory should be observed");
    assert!(
        !staging.exists(),
        "staging directory must be removed after install"
    );
    fs::remove_dir_all(root).expect("temporary directory should be removed");
}

#[test]
fn driver_installer_skips_adb_ids_when_pnputil_returns_nonzero() {
    let root = temporary_directory("driver-install-failure");
    let extractor = FixtureExtractor::with_inf();
    let executor = RecordingElevatedExecutor::with_exit_code(5);
    let adb_ini = root.join(".android").join("adb_usb.ini");
    let installer = DriverInstaller::with_dependencies(
        PathBuf::from("bundle.7z"),
        root.join("staging"),
        adb_ini.clone(),
        extractor,
        executor,
    );

    assert_eq!(
        installer
            .install()
            .expect("nonzero exit should be returned"),
        5
    );
    assert!(
        !adb_ini.exists(),
        "failed installation must not write adb_usb.ini"
    );
    fs::remove_dir_all(root).expect("temporary directory should be removed");
}

#[test]
fn driver_installer_cancels_before_elevated_execution_and_cleans_staging() {
    let root = temporary_directory("driver-install-cancel");
    let extractor = FixtureExtractor::with_inf();
    let executor = RecordingElevatedExecutor::with_exit_code(0);
    let installer = DriverInstaller::with_dependencies(
        PathBuf::from("bundle.7z"),
        root.join("staging"),
        root.join(".android").join("adb_usb.ini"),
        extractor.clone(),
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
    let staging = extractor
        .staging()
        .expect("staging directory should be observed");
    assert!(
        !staging.exists(),
        "staging directory must be removed after cancellation"
    );
    fs::remove_dir_all(root).expect("temporary directory should be removed");
}

#[derive(Clone)]
struct FixtureExtractor {
    write_inf: bool,
    staging: Arc<Mutex<Option<PathBuf>>>,
}

impl FixtureExtractor {
    fn with_inf() -> Self {
        Self {
            write_inf: true,
            staging: Arc::new(Mutex::new(None)),
        }
    }

    fn staging(&self) -> Option<PathBuf> {
        self.staging
            .lock()
            .expect("staging lock should not be poisoned")
            .clone()
    }
}

impl DriverArchiveExtractor for FixtureExtractor {
    fn extract(
        &self,
        _archive: &Path,
        destination: &Path,
    ) -> Result<(), nwflash_domain::DomainError> {
        *self
            .staging
            .lock()
            .expect("staging lock should not be poisoned") = Some(destination.to_path_buf());
        if self.write_inf {
            fs::write(destination.join("driver.inf"), "fixture")
                .expect("fixture inf should be written");
        }
        Ok(())
    }
}

#[derive(Clone)]
struct RecordingElevatedExecutor {
    exit_code: i32,
    command: Arc<Mutex<Option<ProcessCommand>>>,
}

impl RecordingElevatedExecutor {
    fn with_exit_code(exit_code: i32) -> Self {
        Self {
            exit_code,
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
            stdout: String::new(),
            stderr: String::new(),
        })
    }
}
