//! Read-only Windows driver capability detection.

use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    fs,
    io::{self, Read, Seek},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use nwflash_domain::DomainError;
use sha2::{Digest, Sha256};

use crate::process::{ProcessCommand, ProcessOutput};

const ADB_MARKER: &str = "android_winusb";
const FASTBOOT_MARKER: &str = "android_usb";
const MEDIATEK_MARKER: &str = "cdc-acm";
const VIVO_ADB_IDS: [&str; 4] = ["0x2D95", "0x9BB5", "0x18D1", "0x0E8D"];
const BUNDLED_DRIVER_ARCHIVE_FILE_NAME: &str = "vivo-usb-driver.7z";
/// Release-reviewed digest compiled into the desktop binary.
///
/// The runtime must not trust a manifest or sidecar stored beside the writable
/// installed resource. Release tooling separately binds this value to
/// `packaging/release/tauri-resources.json`.
const BUNDLED_DRIVER_ARCHIVE_SHA256: &str =
    "22FA20B21004A7AE76668716EF51E22FD9E8E9EEEA226A035AD23157441B60EA";
static DRIVER_STAGING_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct LockedStagingDirectory {
    path: PathBuf,
    extracted_path: PathBuf,
    _root_guard: fs::File,
    _guard: fs::File,
    _extracted_guard: fs::File,
}

pub trait ElevatedProcessExecutor: Send + Sync {
    fn run_elevated(&self, command: ProcessCommand) -> Result<ProcessOutput, DomainError>;

    /// 在一次提权会话内顺序执行多条命令，返回每条命令的结果。
    ///
    /// 默认实现逐条调用 [`Self::run_elevated`]，语义正确但对用户意味着多次 UAC。
    /// 系统实现覆盖它，把全部命令交给同一个已提权进程执行，用户只授权一次。
    fn run_elevated_batch(
        &self,
        commands: &[ProcessCommand],
    ) -> Result<Vec<ProcessOutput>, DomainError> {
        commands
            .iter()
            .map(|command| self.run_elevated(command.clone()))
            .collect()
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemElevatedProcessExecutor;

/// 一次驱动安装的结果，连同 pnputil 的原始输出。
#[derive(Debug, Clone)]
pub struct InstallOutcome {
    /// pnputil 的退出码（0 或「已存在」的 5 都归一成 0）。
    pub exit_code: i32,
    /// pnputil 输出的合并文本（已从本地代码页解码）。
    /// 提权路径下为空表示未能回收输出，不代表 pnputil 没打印东西。
    pub output: String,
    /// `Added driver packages:` 报告的新增包数。`None` 表示输出里没这一行
    /// （输出未回收，或命令在打印汇总前就死了）。
    pub added_packages: Option<u32>,
    /// 输出里是否出现 `Failed to add driver package`。**这是比退出码可靠的
    /// 失败信号** —— 实测某条 INF 报 `Access is denied` 时整条命令仍然退出 5。
    pub reported_failure: bool,
}

/// pnputil 退出码：命令成功处理了 INF，但没有任何包是新增的（例如全部已存在）。
/// 「已存在」对重装场景是成功而非失败——不归一它，重装已装好的驱动会被误报。
const PNPUTIL_EXIT_ALREADY_PRESENT: i32 = 5;

/// 把一条 pnputil 命令的退出码归一成「成功 / 失败」两态里的成功。
/// 目前只有「已存在」（5）算成功；0 本来就是成功。
fn pnputil_command_succeeded(exit_code: i32) -> bool {
    exit_code == 0 || exit_code == PNPUTIL_EXIT_ALREADY_PRESENT
}

/// pnputil 的退出码语义（实测）：
/// * `0`  命令完成，且至少新增了一个包；
/// * `5`  命令**完成**，但没有包是新增的（例如全部已存在）；
/// * `2`  目标 INF 缺失或非法；
/// * `1`  用法错误（例如 `/add-driver` 收到多个显式 INF 路径）。
/// * `0xE000024B`  **设备绑定阶段**的 CONFIGRET 失败（facility=0、severity=3，
///   不是 Win32 码）。逐条喂单个 INF 时会命中，见
///   [`build_pnputil_install_commands`]。
///
/// **退出码 5 不足以证明成功**：实测 `fastboot_dri_win7` 明确报
/// `Failed to add driver package: Access is denied` 时，整体退出码仍是 5
/// （非提权）或 0（提权）。所以这里除了退出码，还要看输出里有没有失败行。
pub fn driver_install_succeeded(outcome: &InstallOutcome) -> bool {
    outcome.exit_code == 0 && !outcome.reported_failure
}

/// 从一次安装结果中提取可用于展示给用户的失败原因。
pub fn driver_install_failure_detail(outcome: &InstallOutcome) -> String {
    let text = outcome.output.trim();
    if text.is_empty() {
        format!("pnputil 退出码 {}。", outcome.exit_code)
    } else {
        format!("pnputil 退出码 {}：{text}", outcome.exit_code)
    }
}

/// 把多条命令的结果合成一个结论。
///
/// 「已存在」（5）按成功处理；其余退出码取第一条失败命令的，只要有一条
/// 真失败，整体就是失败。输出全部保留，便于定位到底是哪个 INF 出的问题。
///
/// 同时扫描输出里的 `Failed to add driver package` 与 `Added driver packages:`
/// 计数：退出码 5/0 都可能掩盖**个别** INF 的失败，光看退出码会把那种情况
/// 报成成功。
fn merge_install_outcomes(outputs: &[ProcessOutput]) -> InstallOutcome {
    let mut exit_code = 0;
    let mut sections = Vec::new();
    let mut reported_failure = false;
    let mut added_packages = None;
    for output in outputs {
        if !pnputil_command_succeeded(output.exit_code) && exit_code == 0 {
            exit_code = output.exit_code;
        }
        let text = driver_tool_output(output);
        if text.contains("Failed to add driver package") {
            reported_failure = true;
        }
        if let Some(count) = parse_added_driver_packages(&text) {
            added_packages = Some(added_packages.unwrap_or(0) + count);
        }
        if !text.is_empty() {
            sections.push(text);
        }
    }
    InstallOutcome {
        exit_code,
        output: sections.join("\n"),
        added_packages,
        reported_failure,
    }
}

/// 解析 pnputil 汇总行 `Added driver packages:  N`。
///
/// 这是比退出码可靠的「到底装进去几个包」的计数：实测逐条形态下 8 次调用
/// 全部返回 5（含那次 `Access is denied`），而汇总行才是真实结果。
fn parse_added_driver_packages(text: &str) -> Option<u32> {
    text.lines().find_map(|line| {
        let (label, value) = line.split_once(':')?;
        label
            .trim()
            .eq_ignore_ascii_case("Added driver packages")
            .then(|| value.trim().parse::<u32>().ok())
            .flatten()
    })
}

/// 合并 stdout/stderr 为一段文本，过滤空串并保持原有顺序。
fn driver_tool_output(output: &ProcessOutput) -> String {
    let mut parts = Vec::new();
    for stream in [&output.stdout, &output.stderr] {
        let trimmed = stream.trim();
        if !trimmed.is_empty() {
            parts.push(trimmed.to_string());
        }
    }
    parts.join("\n")
}

impl ElevatedProcessExecutor for SystemElevatedProcessExecutor {
    fn run_elevated(&self, command: ProcessCommand) -> Result<ProcessOutput, DomainError> {
        run_elevated_process(command)
    }

    fn run_elevated_batch(
        &self,
        commands: &[ProcessCommand],
    ) -> Result<Vec<ProcessOutput>, DomainError> {
        run_elevated_processes(commands)
    }
}

pub struct DriverInstaller<E = SystemElevatedProcessExecutor> {
    archive_path: PathBuf,
    staging_root: PathBuf,
    adb_usb_ini_path: PathBuf,
    executor: E,
}

impl DriverInstaller {
    pub fn new(archive_path: PathBuf, adb_usb_ini_path: PathBuf) -> Self {
        Self::with_dependencies(
            archive_path,
            std::env::temp_dir().join("NWflash").join("drivers"),
            adb_usb_ini_path,
            SystemElevatedProcessExecutor,
        )
    }
}

pub fn locate_bundled_driver_archive(application_root: &Path) -> Option<PathBuf> {
    let archive = application_root
        .join("drivers")
        .join(BUNDLED_DRIVER_ARCHIVE_FILE_NAME);
    archive.is_file().then_some(archive)
}

impl<E> DriverInstaller<E>
where
    E: ElevatedProcessExecutor,
{
    pub fn with_dependencies(
        archive_path: PathBuf,
        staging_root: PathBuf,
        adb_usb_ini_path: PathBuf,
        executor: E,
    ) -> Self {
        Self {
            archive_path,
            staging_root,
            adb_usb_ini_path,
            executor,
        }
    }

    pub fn install(&self) -> Result<i32, DomainError> {
        self.install_with_cancel(|| false)
    }

    pub fn install_with_cancel<F>(&self, mut should_cancel: F) -> Result<i32, DomainError>
    where
        F: FnMut() -> bool,
    {
        self.install_with_cancel_detailed(&mut should_cancel)
            .map(|outcome| outcome.exit_code)
    }

    /// 与 [`Self::install_with_cancel`] 相同，但保留 pnputil 的原始输出。
    ///
    /// 提权路径下输出只能经日志文件回收（`ShellExecuteExW` 不给管道），失败时
    /// 若不转述，用户和日志里就只剩一个光秃秃的退出码，无法区分「用法错误」
    /// 「INF 被拒」「签名失败」。
    pub fn install_with_cancel_detailed<F>(
        &self,
        mut should_cancel: F,
    ) -> Result<InstallOutcome, DomainError>
    where
        F: FnMut() -> bool,
    {
        let staging = self.create_staging_directory()?;
        let staging_path = staging.path.clone();
        let result = (|| {
            let verified_archive = staging_path.join("verified-driver-archive.7z");
            let archive_guard =
                create_verified_driver_archive_snapshot(&self.archive_path, &verified_archive)?;

            let extracted_guard = staging
                ._extracted_guard
                .try_clone()
                .map_err(|_| driver_archive_integrity_error())?;
            let mut frozen = extract_and_freeze_verified_driver_archive(
                archive_guard,
                &staging.extracted_path,
                extracted_guard,
            )?;
            if should_cancel() {
                return Err(DomainError::UserCancelled("用户取消驱动安装。".to_string()));
            }

            // 一条通配符 + `/subdirs` 命令装完整棵树（含各自的 catalog）。
            // 逐条喂单个 INF 会在设备绑定阶段以 CONFIGRET 0xE000024B 失败，
            // 详见 build_pnputil_install_commands 的说明。命令数恒为 1，
            // batch 接口仍保留：它是提权 + 回收 pnputil 输出的唯一通道。
            frozen.revalidate()?;
            let commands = build_pnputil_install_commands(&frozen.inf_paths)?;
            let outputs = self.executor.run_elevated_batch(&commands)?;
            let outcome = merge_install_outcomes(&outputs);
            if !driver_install_succeeded(&outcome) {
                return Ok(outcome);
            }
            // Modern adb has these VIDs built in, so preserving the successful driver
            // installation result is more important than this compatibility supplement.
            let _ = write_vivo_adb_usb_ids(&self.adb_usb_ini_path);
            Ok(outcome)
        })();
        drop(staging);
        let cleanup = fs::remove_dir_all(&staging_path);
        // 安装已成功时，临时目录清理失败按 C# DeleteQuietly 语义吞掉：
        // 驱动已在 DriverStore 注册成功，目录被占用/只读不能把安装报成失败。
        let _ = cleanup;
        result
    }

    fn create_staging_directory(&self) -> Result<LockedStagingDirectory, DomainError> {
        fs::create_dir_all(&self.staging_root)
            .map_err(|error| DomainError::Internal(format!("创建驱动临时目录失败：{error}")))?;
        reject_reparse_ancestry(&self.staging_root)?;
        let root_guard = open_checked_read_guard(&self.staging_root, true)?;
        for _ in 0..64 {
            let sequence = DRIVER_STAGING_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|error| DomainError::Internal(format!("读取系统时间失败：{error}")))?
                .as_nanos();
            let directory = self.staging_root.join(format!(
                "{}-{nonce:032x}-{sequence:016x}",
                std::process::id()
            ));
            match fs::create_dir(&directory) {
                Ok(()) => {
                    let guard = match open_checked_read_guard(&directory, true) {
                        Ok(guard) => guard,
                        Err(error) => {
                            let _ = fs::remove_dir(&directory);
                            return Err(error);
                        }
                    };
                    let extracted_path = directory.join("extracted");
                    if let Err(error) = fs::create_dir(&extracted_path) {
                        let _ = fs::remove_dir(&directory);
                        return Err(DomainError::Internal(format!(
                            "创建驱动临时目录失败：{error}"
                        )));
                    }
                    let extracted_guard = match open_checked_read_guard(&extracted_path, true) {
                        Ok(guard) => guard,
                        Err(error) => {
                            let _ = fs::remove_dir_all(&directory);
                            return Err(error);
                        }
                    };
                    return Ok(LockedStagingDirectory {
                        path: directory,
                        extracted_path,
                        _root_guard: root_guard,
                        _guard: guard,
                        _extracted_guard: extracted_guard,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(DomainError::Internal(format!(
                        "创建驱动临时目录失败：{error}"
                    )))
                }
            }
        }
        Err(DomainError::Internal(
            "无法创建排他的驱动临时目录。".to_string(),
        ))
    }
}

fn verify_driver_archive_file(
    file: &mut fs::File,
    expected_sha256: &str,
) -> Result<(), DomainError> {
    file.seek(io::SeekFrom::Start(0))
        .map_err(|_| driver_archive_integrity_error())?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| driver_archive_integrity_error())?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let actual = format!("{:X}", hasher.finalize());
    if !actual.eq_ignore_ascii_case(expected_sha256) {
        return Err(driver_archive_integrity_error());
    }
    file.seek(io::SeekFrom::Start(0))
        .map_err(|_| driver_archive_integrity_error())?;
    Ok(())
}

fn create_verified_driver_archive_snapshot(
    source: &Path,
    destination: &Path,
) -> Result<fs::File, DomainError> {
    // Hold the installed resource by handle for the whole snapshot copy. The
    // source path lives in a current-user installation directory, so a later
    // path replacement must neither influence nor race this authenticated copy.
    let mut source = open_checked_read_guard(source, false)?;
    let mut snapshot = create_read_write_deny_write_delete(destination)
        .map_err(|_| driver_archive_integrity_error())?;
    io::copy(&mut source, &mut snapshot).map_err(|_| driver_archive_integrity_error())?;
    snapshot
        .sync_all()
        .map_err(|_| driver_archive_integrity_error())?;
    verify_driver_archive_file(&mut snapshot, BUNDLED_DRIVER_ARCHIVE_SHA256)?;
    Ok(snapshot)
}

struct FrozenDriverTree {
    inf_paths: Vec<PathBuf>,
    files: Vec<FrozenDriverFile>,
    _directory_guards: Vec<fs::File>,
    path_guards: Vec<fs::File>,
}

struct FrozenDriverFile {
    path: PathBuf,
    handle: fs::File,
    identity: FileIdentity,
    length: u64,
    sha256: String,
}

impl FrozenDriverTree {
    fn revalidate(&mut self) -> Result<(), DomainError> {
        self.path_guards.clear();
        for file in &mut self.files {
            let handle_len = file
                .handle
                .metadata()
                .map_err(|_| driver_archive_integrity_error())?
                .len();
            let handle_identity = file_identity(&file.handle)?;
            let handle_hash = hash_open_file(&mut file.handle)?;
            if handle_len != file.length
                || handle_identity != file.identity
                || handle_hash != file.sha256
            {
                return Err(driver_archive_integrity_error());
            }
            let mut path_guard = open_checked_read_guard(&file.path, false)?;
            let path_identity = file_identity(&path_guard)?;
            let path_len = path_guard
                .metadata()
                .map_err(|_| driver_archive_integrity_error())?
                .len();
            let path_hash = hash_open_file(&mut path_guard)?;
            if path_identity != file.identity || path_len != file.length || path_hash != file.sha256
            {
                return Err(driver_archive_integrity_error());
            }
            self.path_guards.push(path_guard);
        }
        Ok(())
    }
}

fn extract_and_freeze_verified_driver_archive(
    archive: fs::File,
    root: &Path,
    root_guard: fs::File,
) -> Result<FrozenDriverTree, DomainError> {
    let archive_len = archive
        .metadata()
        .map_err(|_| driver_archive_integrity_error())?
        .len();
    let mut reader =
        sevenz_rust::SevenZReader::new(archive, archive_len, sevenz_rust::Password::empty())
            .map_err(|_| driver_archive_integrity_error())?;
    let mut expected_files = BTreeMap::<PathBuf, u64>::new();
    let mut expected_directories = BTreeSet::<PathBuf>::new();
    for entry in &reader.archive().files {
        if entry.is_anti_item() {
            return Err(driver_archive_integrity_error());
        }
        let Some(relative) = safe_archive_entry_path(entry.name(), entry.is_directory())
            .map_err(|_| driver_archive_integrity_error())?
        else {
            continue;
        };
        if entry.is_directory() {
            expected_directories.insert(relative.clone());
        } else if expected_files
            .insert(relative.clone(), entry.size())
            .is_some()
        {
            return Err(driver_archive_integrity_error());
        }
        let mut parent = relative.parent();
        while let Some(directory) = parent {
            if !directory.as_os_str().is_empty() {
                expected_directories.insert(directory.to_path_buf());
            }
            parent = directory.parent();
        }
    }
    if expected_files.is_empty() {
        return Err(driver_archive_integrity_error());
    }

    let mut directory_guards = vec![root_guard];
    let mut directories = expected_directories.iter().cloned().collect::<Vec<_>>();
    directories.sort_by_key(|path| path.components().count());
    for relative in &directories {
        let path = root.join(relative);
        fs::create_dir(&path).map_err(|_| driver_archive_integrity_error())?;
        directory_guards.push(open_checked_read_guard(&path, true)?);
    }

    let canonical_root = root
        .canonicalize()
        .map_err(|_| driver_archive_integrity_error())?;
    let mut files = Vec::new();
    let mut seen = BTreeSet::new();
    reader
        .for_each_entries(|entry, contents| {
            let Some(relative) = safe_archive_entry_path(entry.name(), entry.is_directory())?
            else {
                return Ok(true);
            };
            if entry.is_directory() {
                if !expected_directories.contains(&relative) {
                    return Err(sevenz_rust::Error::other("unexpected driver directory"));
                }
                return Ok(true);
            }
            let expected_size = expected_files
                .get(&relative)
                .copied()
                .ok_or_else(|| sevenz_rust::Error::other("unexpected driver file"))?;
            if !seen.insert(relative.clone()) {
                return Err(sevenz_rust::Error::other("duplicate driver file"));
            }
            let path = root.join(&relative);
            let mut handle =
                create_read_write_deny_write_delete(&path).map_err(sevenz_rust::Error::io)?;
            let written = io::copy(contents, &mut handle).map_err(sevenz_rust::Error::io)?;
            handle.sync_all().map_err(sevenz_rust::Error::io)?;
            if written != expected_size {
                return Err(sevenz_rust::Error::other("driver file size mismatch"));
            }
            let canonical = path.canonicalize().map_err(sevenz_rust::Error::io)?;
            if !canonical.starts_with(&canonical_root) {
                return Err(sevenz_rust::Error::other("driver path escaped staging"));
            }
            let identity = file_identity(&handle)
                .map_err(|_| sevenz_rust::Error::other("driver file identity failed"))?;
            let sha256 = hash_open_file(&mut handle)
                .map_err(|_| sevenz_rust::Error::other("driver file hash failed"))?;
            let handle = downgrade_write_handle_to_read_guard(&canonical, handle, identity)
                .map_err(|_| sevenz_rust::Error::other("driver file freeze failed"))?;
            files.push(FrozenDriverFile {
                path: canonical,
                handle,
                identity,
                length: written,
                sha256,
            });
            Ok(true)
        })
        .map_err(|_| driver_archive_integrity_error())?;
    if seen != expected_files.keys().cloned().collect() {
        return Err(driver_archive_integrity_error());
    }
    verify_extracted_tree_matches_archive(root, &expected_files, &expected_directories)?;

    let mut inf_paths = expected_files
        .keys()
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("inf"))
        })
        .map(|path| root.join(path).canonicalize())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| driver_archive_integrity_error())?;
    if inf_paths.is_empty() {
        return Err(DomainError::InvalidFormat(
            "驱动包内未找到任何 INF，请重新下载安装包。".to_string(),
        ));
    }
    inf_paths.sort();
    let mut frozen = FrozenDriverTree {
        inf_paths,
        files,
        _directory_guards: directory_guards,
        path_guards: Vec::new(),
    };
    frozen.revalidate()?;
    Ok(frozen)
}

fn verify_extracted_tree_matches_archive(
    root: &Path,
    expected_files: &BTreeMap<PathBuf, u64>,
    expected_directories: &BTreeSet<PathBuf>,
) -> Result<(), DomainError> {
    fn visit(
        root: &Path,
        directory: &Path,
        files: &mut BTreeSet<PathBuf>,
        directories: &mut BTreeSet<PathBuf>,
    ) -> Result<(), DomainError> {
        for entry in fs::read_dir(directory).map_err(|_| driver_archive_integrity_error())? {
            let path = entry.map_err(|_| driver_archive_integrity_error())?.path();
            reject_reparse_path(&path)?;
            let relative = path
                .strip_prefix(root)
                .map_err(|_| driver_archive_integrity_error())?
                .to_path_buf();
            let metadata =
                fs::symlink_metadata(&path).map_err(|_| driver_archive_integrity_error())?;
            if metadata.is_dir() {
                directories.insert(relative);
                visit(root, &path, files, directories)?;
            } else if metadata.is_file() {
                files.insert(relative);
            } else {
                return Err(driver_archive_integrity_error());
            }
        }
        Ok(())
    }

    let mut files = BTreeSet::new();
    let mut directories = BTreeSet::new();
    visit(root, root, &mut files, &mut directories)?;
    if files != expected_files.keys().cloned().collect() || directories != *expected_directories {
        return Err(driver_archive_integrity_error());
    }
    Ok(())
}

fn hash_open_file(file: &mut fs::File) -> Result<String, DomainError> {
    file.seek(io::SeekFrom::Start(0))
        .map_err(|_| driver_archive_integrity_error())?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| driver_archive_integrity_error())?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    file.seek(io::SeekFrom::Start(0))
        .map_err(|_| driver_archive_integrity_error())?;
    Ok(format!("{:X}", hasher.finalize()))
}

fn reject_reparse_ancestry(path: &Path) -> Result<(), DomainError> {
    for ancestor in path.ancestors() {
        if ancestor.exists() {
            reject_reparse_path(ancestor)?;
        }
    }
    Ok(())
}

fn reject_reparse_path(path: &Path) -> Result<(), DomainError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| driver_archive_integrity_error())?;
    if metadata.file_type().is_symlink() || metadata_is_reparse_point(&metadata) {
        return Err(driver_archive_integrity_error());
    }
    Ok(())
}

fn open_checked_read_guard(path: &Path, directory: bool) -> Result<fs::File, DomainError> {
    let file = open_read_deny_write_delete(path, directory)
        .map_err(|_| driver_archive_integrity_error())?;
    let metadata = file
        .metadata()
        .map_err(|_| driver_archive_integrity_error())?;
    if metadata_is_reparse_point(&metadata) {
        return Err(driver_archive_integrity_error());
    }
    Ok(file)
}

fn downgrade_write_handle_to_read_guard(
    path: &Path,
    write_handle: fs::File,
    expected_identity: FileIdentity,
) -> Result<fs::File, DomainError> {
    let transition = open_transition_read_guard(path)?;
    if file_identity(&transition)? != expected_identity {
        return Err(driver_archive_integrity_error());
    }
    drop(write_handle);
    let final_guard = open_checked_read_guard(path, false)?;
    if file_identity(&final_guard)? != expected_identity {
        return Err(driver_archive_integrity_error());
    }
    drop(transition);
    Ok(final_guard)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    volume_serial: u32,
    file_index: u64,
}

#[cfg(windows)]
fn file_identity(file: &fs::File) -> Result<FileIdentity, DomainError> {
    use std::os::windows::io::AsRawHandle;

    #[repr(C)]
    struct FileTime {
        low: u32,
        high: u32,
    }

    #[repr(C)]
    struct ByHandleFileInformation {
        attributes: u32,
        creation_time: FileTime,
        last_access_time: FileTime,
        last_write_time: FileTime,
        volume_serial: u32,
        file_size_high: u32,
        file_size_low: u32,
        number_of_links: u32,
        file_index_high: u32,
        file_index_low: u32,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetFileInformationByHandle(
            file: *mut std::ffi::c_void,
            information: *mut ByHandleFileInformation,
        ) -> i32;
    }

    let mut information = std::mem::MaybeUninit::<ByHandleFileInformation>::uninit();
    let success = unsafe {
        GetFileInformationByHandle(file.as_raw_handle().cast(), information.as_mut_ptr())
    };
    if success == 0 {
        return Err(driver_archive_integrity_error());
    }
    let information = unsafe { information.assume_init() };
    Ok(FileIdentity {
        volume_serial: information.volume_serial,
        file_index: (u64::from(information.file_index_high) << 32)
            | u64::from(information.file_index_low),
    })
}

#[cfg(not(windows))]
fn file_identity(file: &fs::File) -> Result<FileIdentity, DomainError> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file
        .metadata()
        .map_err(|_| driver_archive_integrity_error())?;
    Ok(FileIdentity {
        volume_serial: metadata.dev() as u32,
        file_index: metadata.ino(),
    })
}

#[cfg(windows)]
fn metadata_is_reparse_point(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn metadata_is_reparse_point(_metadata: &fs::Metadata) -> bool {
    false
}

#[cfg(windows)]
fn open_read_deny_write_delete(path: &Path, directory: bool) -> io::Result<fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    let mut options = fs::OpenOptions::new();
    options
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    if directory {
        options
            .write(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options.open(path)
}

#[cfg(windows)]
fn open_transition_read_guard(path: &Path) -> Result<fs::File, DomainError> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_SHARE_WRITE: u32 = 0x0000_0002;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    let mut options = fs::OpenOptions::new();
    let file = options
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|_| driver_archive_integrity_error())?;
    if metadata_is_reparse_point(
        &file
            .metadata()
            .map_err(|_| driver_archive_integrity_error())?,
    ) {
        return Err(driver_archive_integrity_error());
    }
    Ok(file)
}

#[cfg(not(windows))]
fn open_read_deny_write_delete(path: &Path, _directory: bool) -> io::Result<fs::File> {
    fs::File::open(path)
}

#[cfg(not(windows))]
fn open_transition_read_guard(path: &Path) -> Result<fs::File, DomainError> {
    fs::File::open(path).map_err(|_| driver_archive_integrity_error())
}

#[cfg(windows)]
fn create_read_write_deny_write_delete(path: &Path) -> io::Result<fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    let mut options = fs::OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create_new(true)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

#[cfg(not(windows))]
fn create_read_write_deny_write_delete(path: &Path) -> io::Result<fs::File> {
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)
}

fn driver_archive_integrity_error() -> DomainError {
    DomainError::ExternalTool("内置 USB 驱动包完整性校验失败，请重新安装奶蛙Flash。".to_string())
}

#[derive(Debug, Clone)]
pub struct DriverDetectionPaths {
    driver_store_directories: Vec<PathBuf>,
    legacy_install_directories: Vec<PathBuf>,
    /// 是否探测旧版驱动的卸载注册表键。生产 `default_windows()` 为 true;
    /// 测试用 `new()` 注入临时目录时为 false——单测的意图是验证目录标记
    /// 语义,不应随宿主机是否装过 vivo 驱动而摇摆。
    probe_legacy_registry: bool,
}

impl DriverDetectionPaths {
    pub fn new(
        driver_store_directories: Vec<PathBuf>,
        legacy_install_directories: Vec<PathBuf>,
    ) -> Self {
        Self {
            driver_store_directories,
            legacy_install_directories,
            probe_legacy_registry: false,
        }
    }

    pub fn default_windows() -> Self {
        let windows = std::env::var_os("WINDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        let driver_store = windows
            .join("System32")
            .join("DriverStore")
            .join("FileRepository");
        let mut legacy_install_directories = Vec::new();
        for variable in ["ProgramFiles", "ProgramFiles(x86)"] {
            if let Some(root) = std::env::var_os(variable).map(PathBuf::from) {
                let candidate = root.join("BBK").join("vivo_usb_driver");
                if !legacy_install_directories.contains(&candidate) {
                    legacy_install_directories.push(candidate);
                }
            }
        }

        Self {
            driver_store_directories: vec![driver_store],
            legacy_install_directories,
            probe_legacy_registry: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DriverStatus {
    pub adb_installed: bool,
    pub fastboot_installed: bool,
    pub mediatek_installed: bool,
}

impl DriverStatus {
    pub fn all_installed(&self) -> bool {
        self.adb_installed && self.fastboot_installed && self.mediatek_installed
    }
}

pub fn detect_drivers(paths: &DriverDetectionPaths) -> DriverStatus {
    // 三路信号（与 C# VivoDriverDetector 一致）：DriverStore 标记、旧版
    // 全量驱动包安装目录、旧版卸载注册表键（目录可能被卸载残留清空，
    // 注册表键仍在也算已安装，避免对旧包机器误报未安装反复提醒）。
    let legacy_installed = paths
        .legacy_install_directories
        .iter()
        .any(|path| contains_inf_recursively(path))
        || (paths.probe_legacy_registry && legacy_driver_uninstall_registry_key_exists());

    DriverStatus {
        adb_installed: legacy_installed
            || has_driver_store_marker(&paths.driver_store_directories, ADB_MARKER),
        fastboot_installed: legacy_installed
            || has_driver_store_marker(&paths.driver_store_directories, FASTBOOT_MARKER),
        mediatek_installed: legacy_installed
            || has_mediatek_driver(&paths.driver_store_directories),
    }
}

/// 旧版 vivo 全量驱动包的 Inno Setup 卸载注册表键
/// （`Uninstall\vivo_usb_driver_is1`，含 WOW6432Node 视图）。
const LEGACY_DRIVER_UNINSTALL_REGISTRY_KEYS: [&str; 2] = [
    r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\vivo_usb_driver_is1",
    r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\vivo_usb_driver_is1",
];

#[cfg(windows)]
fn legacy_driver_uninstall_registry_key_exists() -> bool {
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, HKEY_LOCAL_MACHINE, KEY_READ,
    };

    LEGACY_DRIVER_UNINSTALL_REGISTRY_KEYS.iter().any(|subkey| {
        let mut wide: Vec<u16> = subkey.encode_utf16().collect();
        wide.push(0);
        let mut key = std::ptr::null_mut();
        // KEY_READ 足够判断键存在；不存在/无权限都按“未安装”处理。
        let opened =
            unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, wide.as_ptr(), 0, KEY_READ, &mut key) };
        if opened == 0 {
            unsafe { RegCloseKey(key) };
            true
        } else {
            false
        }
    })
}

#[cfg(not(windows))]
fn legacy_driver_uninstall_registry_key_exists() -> bool {
    false
}

/// 构造安装命令：`/add-driver "<staging>\*.inf" /subdirs /install`——**一条**
/// 通配符递归命令，和 C# 版（能在这台机器上装成功的那版）完全一致。
///
/// 为什么必须是这个形态（2026-09-28 实测，逐条踩过两个坑）：
///
/// * 逐条喂**单个 INF 绝对路径**（`/add-driver <path> /install`）×8 条，会在
///   **设备绑定阶段**炸掉并返回 `0xE000024B`——那是 CONFIGRET 域的码
///   （severity=3/facility=0），不是 Win32。原因是单独喂一个 INF 时 pnputil
///   找不到配套的 `.cat` 目录文件，绑设备时 catalog 校验过不去。报错长这样：
///   `Driver package installed on device: USB\VID_…` 之后返回该码。
/// * 一条命令里塞**多个显式 INF 路径**（`/add-driver a.inf b.inf`）会被整行
///   拒绝（打印用法、退出码 1）。所以「多处指定」走不通，只有通配符可以。
///
/// `/subdirs` 让 pnputil 把 staging 当**一棵驱动包树**处理，8 个子目录里的 INF
/// 连同各自的 catalog 一起被正确解析。实测这条命令 `Added driver packages: 7`、
/// 正常退出；同一次运行里逐条形态则精确复现用户的 `-536870325`。
///
/// 通配符是**我们自己拼的固定模式**（`<已校验的 staging 根>\*.inf`），不是外部
/// 输入：`inf_paths` 来自解包后逐文件哈希校验过的树，这里只用它们确认
/// 「至少有一个 INF」并定位 common root，绝不把条数交给自己去逐个展开。
fn build_pnputil_install_commands(infs: &[PathBuf]) -> Result<Vec<ProcessCommand>, DomainError> {
    if infs.is_empty() {
        return Err(DomainError::InvalidInput(
            "驱动安装缺少 INF 目标。".to_string(),
        ));
    }
    // 每个 INF 都要先过读取守卫与形态校验，保持与逐条形态同等的输入把关；
    // 随后只取它们的**最深公共祖先**作为通配符根。
    let mut canonical_infs = Vec::with_capacity(infs.len());
    for inf in infs {
        let file_name = inf
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if !inf.is_absolute()
            || file_name.contains(['*', '?'])
            || inf
                .extension()
                .is_none_or(|extension| !extension.eq_ignore_ascii_case("inf"))
        {
            return Err(DomainError::InvalidInput(
                "驱动安装目标必须是一个 INF 文件。".to_string(),
            ));
        }
        let _guard = open_checked_read_guard(inf, false)?;
        let inf = inf
            .canonicalize()
            .map_err(|_| driver_archive_integrity_error())?;
        canonical_infs.push(inf);
    }
    let wildcard_root = common_directory_root(&canonical_infs)?;
    let pattern = wildcard_root.join("*.inf");

    let system_directory = system_directory_path()?;
    let pnputil = system_directory.join("pnputil.exe");
    #[cfg(windows)]
    {
        let metadata =
            fs::symlink_metadata(&pnputil).map_err(|_| driver_archive_integrity_error())?;
        if !metadata.is_file() || metadata_is_reparse_point(&metadata) {
            return Err(driver_archive_integrity_error());
        }
        let guard = open_checked_read_guard(&pnputil, false)?;
        if !guard
            .metadata()
            .map_err(|_| driver_archive_integrity_error())?
            .is_file()
        {
            return Err(driver_archive_integrity_error());
        }
    }
    let program = display_path_for_external_tool(&pnputil);
    Ok(vec![ProcessCommand::new(
        program,
        vec![
            "/add-driver".to_string(),
            display_path_for_external_tool(&pattern),
            "/subdirs".to_string(),
            "/install".to_string(),
        ],
    )])
}

/// 取一组已 canonicalize 的 INF 路径的**最深公共祖先目录**。
///
/// 驱动包把 INF 放在 `adbinfs_win10/`、`fastboot_dri_win7/` 这类子目录里，
/// 通配符根因此落在解包根（staging 的 `extracted`）上，配合 `/subdirs`
/// 覆盖全部子目录。全部同目录时退化成该目录本身。
fn common_directory_root(paths: &[PathBuf]) -> Result<PathBuf, DomainError> {
    let mut root = paths[0]
        .parent()
        .ok_or_else(driver_archive_integrity_error)?
        .to_path_buf();
    for path in &paths[1..] {
        let parent = path.parent().ok_or_else(driver_archive_integrity_error)?;
        while !parent.starts_with(&root) {
            root = match root.parent() {
                Some(parent_of_root) => parent_of_root.to_path_buf(),
                None => return Err(driver_archive_integrity_error()),
            };
        }
    }
    Ok(root)
}

/// 把路径转成可以交给外部工具（pnputil / cmd）的文本形态。
///
/// `std::fs::canonicalize` 在 Windows 上返回 `\\?\C:\…` 这种 verbatim 前缀路径。
/// 本进程内用它做文件操作没问题，但**传给 pnputil 会被拒**：实测 pnputil 收到带
/// 前缀的 INF 路径时报「系统找不到指定的路径」并以退出码 1 结束。这里剥掉前缀，
/// 还原成普通 `C:\…` 形态。路径在 canonicalize 之后已消解过相对段与 `.`/`..`，
/// 剥前缀不会重新引入歧义。
fn display_path_for_external_tool(path: &Path) -> String {
    let text = path.to_string_lossy();
    // `\\?\UNC\server\share` 要还原成 `\\server\share`，不能只靠剥前缀。
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        return rest.to_string();
    }
    text.into_owned()
}

#[cfg(windows)]
fn system_directory_path() -> Result<PathBuf, DomainError> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;

    let mut buffer = vec![0_u16; 260];
    loop {
        let length = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) };
        if length == 0 {
            return Err(driver_archive_integrity_error());
        }
        if (length as usize) < buffer.len() {
            let path = std::ffi::OsString::from_wide(&buffer[..length as usize]);
            let path = PathBuf::from(path);
            reject_reparse_ancestry(&path)?;
            let metadata =
                fs::symlink_metadata(&path).map_err(|_| driver_archive_integrity_error())?;
            if !metadata.is_dir() || metadata_is_reparse_point(&metadata) {
                return Err(driver_archive_integrity_error());
            }
            return path
                .canonicalize()
                .map_err(|_| driver_archive_integrity_error());
        }
        buffer.resize(length as usize + 1, 0);
    }
}

#[cfg(not(windows))]
fn system_directory_path() -> Result<PathBuf, DomainError> {
    Ok(PathBuf::from(r"C:\Windows\System32"))
}

pub fn write_vivo_adb_usb_ids(path: &Path) -> Result<(), DomainError> {
    let parent = path
        .parent()
        .ok_or_else(|| DomainError::InvalidInput("adb_usb.ini 路径缺少父目录。".to_string()))?;
    fs::create_dir_all(parent)
        .map_err(|error| DomainError::Internal(format!("创建 adb 配置目录失败：{error}")))?;

    let existing = match fs::read_to_string(path) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(DomainError::Internal(format!(
                "读取 adb_usb.ini 失败：{error}"
            )))
        }
    };
    let present: HashSet<String> = existing
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("0x"))
        .map(|line| line.to_ascii_lowercase())
        .collect();
    let missing = VIVO_ADB_IDS
        .iter()
        .filter(|id| !present.contains(&id.to_ascii_lowercase()))
        .copied()
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(());
    }

    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| DomainError::Internal(format!("写入 adb_usb.ini 失败：{error}")))?;
    for id in missing {
        writeln!(file, "{id}")
            .map_err(|error| DomainError::Internal(format!("写入 adb_usb.ini 失败：{error}")))?;
    }
    Ok(())
}

fn has_driver_store_marker(directories: &[PathBuf], marker: &str) -> bool {
    directories.iter().any(|directory| {
        fs::read_dir(directory)
            .ok()
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .any(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .split('.')
                    .next()
                    .is_some_and(|name| name.eq_ignore_ascii_case(marker))
            })
    })
}

fn has_mediatek_driver(directories: &[PathBuf]) -> bool {
    directories.iter().any(|directory| {
        fs::read_dir(directory)
            .ok()
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(MEDIATEK_MARKER)
            })
            .any(|entry| {
                fs::read_dir(entry.path())
                    .ok()
                    .into_iter()
                    .flatten()
                    .filter_map(Result::ok)
                    .filter(|file| {
                        file.path()
                            .extension()
                            .is_some_and(|extension| extension.eq_ignore_ascii_case("inf"))
                    })
                    .any(|file| {
                        fs::read_to_string(file.path())
                            .ok()
                            .is_some_and(|content| content.to_lowercase().contains("mediatek"))
                    })
            })
    })
}

fn contains_inf_recursively(path: &Path) -> bool {
    let Ok(entries) = fs::read_dir(path) else {
        return false;
    };

    entries.filter_map(Result::ok).any(|entry| {
        let path = entry.path();
        if path.is_dir() {
            contains_inf_recursively(&path)
        } else {
            path.extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("inf"))
        }
    })
}

fn safe_archive_entry_path(
    name: &str,
    is_directory: bool,
) -> Result<Option<PathBuf>, sevenz_rust::Error> {
    let entry_path = Path::new(name);
    let mut relative = PathBuf::new();
    for component in entry_path.components() {
        match component {
            Component::Normal(segment) => relative.push(segment),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(sevenz_rust::Error::other(
                    "driver archive contains an unsafe path entry",
                ));
            }
        }
    }
    if relative.as_os_str().is_empty() {
        if is_directory {
            return Ok(None);
        }
        return Err(sevenz_rust::Error::other(
            "driver archive contains an empty path entry",
        ));
    }
    Ok(Some(relative))
}

/// 读取提权进程留下的输出并删除日志文件。
///
/// 提权子进程把输出写进文件时按其控制台代码页编码（中文系统 GBK/936，
/// 英文系统 1252/437），先按 UTF-8 试（65001 系统），失败再按系统 ANSI
/// 代码页宽松解码；解码不了的字节保留替换字符，绝不因为解码问题丢掉
/// 诊断信息本身。
#[cfg(windows)]
fn take_elevated_output_log(path: &Path) -> String {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(_) => return String::new(),
    };
    let _ = fs::remove_file(path);
    decode_elevated_output(&bytes)
}

/// 按系统 ANSI 代码页解码提权输出。固定 GBK 会把英文系统（1252）下的
/// 输出解成乱码，只保得住 ASCII 骨架；这里按真实代码页选解码器。
#[cfg(windows)]
fn decode_elevated_output(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.to_string();
    }
    // SAFETY: `GetACP` 无参数、无失败路径，任意时刻可调用。
    let code_page = unsafe { windows_sys::Win32::Globalization::GetACP() };
    let encoding = match code_page {
        936 => encoding_rs::GBK,
        950 => encoding_rs::BIG5,
        932 => encoding_rs::SHIFT_JIS,
        949 => encoding_rs::EUC_KR,
        _ => encoding_rs::WINDOWS_1252,
    };
    let (decoded, _, _) = encoding.decode(bytes);
    decoded.into_owned()
}

#[cfg(windows)]
fn run_elevated_process(command: ProcessCommand) -> Result<ProcessOutput, DomainError> {
    let mut outputs = run_elevated_processes(&[command])?;
    outputs
        .pop()
        .ok_or_else(|| DomainError::ExternalTool("提权进程未返回结果。".to_string()))
}

/// 在**一次**提权会话里顺序执行多条命令。
///
/// `ShellExecuteExW` + `runas` 每次调用都会弹一次 UAC，而驱动包装了 8 个 INF、
/// 每个都要单独一条 pnputil 命令，逐条提权会让用户连点 8 次。这里把全部命令
/// 交给同一个提权 `cmd` 会话顺序执行，用户只授权一次。
///
/// 每条命令的输出写进各自的临时文件，退出码写进一个状态文件 —— `ShellExecuteExW`
/// 不给管道，这是唯一能把结果带回来的方式。
#[cfg(windows)]
fn run_elevated_processes(commands: &[ProcessCommand]) -> Result<Vec<ProcessOutput>, DomainError> {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, WAIT_OBJECT_0, WAIT_TIMEOUT},
        System::Threading::WaitForSingleObject,
        UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW},
    };

    if commands.is_empty() {
        return Err(DomainError::InvalidInput("提权批次不能为空。".to_string()));
    }
    for command in commands {
        crate::process::validate_command(&command.program)?;
        crate::process::validate_args(&command.args)?;
        if command.working_directory.is_some() || !command.environment.is_empty() {
            return Err(DomainError::InvalidInput(
                "驱动安装命令不支持工作目录或环境变量。".to_string(),
            ));
        }
    }

    let scratch = elevated_scratch_directory();
    fs::create_dir_all(&scratch)
        .map_err(|error| DomainError::Internal(format!("创建提权临时目录失败：{error}")))?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let log_paths: Vec<PathBuf> = (0..commands.len())
        .map(|index| scratch.join(format!("install-{nonce:032x}-{index}.log")))
        .collect();
    let status_path = scratch.join(format!("install-{nonce:032x}-status.txt"));

    // 每条命令：执行 → 把退出码追加进状态文件 → 输出重定向到各自日志。
    // 全部用 `&` 串成**一行**，绝不插换行：`/s` 形态下 cmd 只把第一个换行前的
    // 内容当命令，后面的行会被丢弃（实测 8 条命令只有第 1 条执行，状态文件里
    // 只有一行）。命令之间不短路，某一条失败后仍继续，用户一次就能看到全部问题。
    //
    // `%ERRORLEVEL%` 与 `>>` 之间**必须有空格**：写成 `%ERRORLEVEL%>>` 时 cmd 会把
    // 重定向符吞进变量名解析里，状态文件根本不生成（实测）。
    // 变量还必须用 `!ERRORLEVEL!` 配 `/v:on`（延迟展开）：用 `%ERRORLEVEL%` 时
    // cmd 会在**整条脚本解析时**一次性替换，所有条目都记成同一个值（实测记成 0）。
    let mut script = String::new();
    for (index, (command, log)) in commands.iter().zip(&log_paths).enumerate() {
        let target = quote_windows_argument(&log.to_string_lossy());
        script.push_str(&format!(
            "{} {} > {target} 2>&1 & echo {index} !ERRORLEVEL! >> {} & ",
            quote_windows_argument(&command.program),
            windows_command_line(&command.args),
            quote_windows_argument(&status_path.to_string_lossy()),
        ));
    }
    // 去掉尾部的 ` & `，避免 cmd 报「命令语法不正确」。
    let script = script
        .trim_end()
        .trim_end_matches('&')
        .trim_end()
        .to_string();

    let batch_program = wide_null(&std::env::var("COMSPEC").unwrap_or_else(|_| {
        system_directory_path()
            .map(|directory| directory.join("cmd.exe").to_string_lossy().into_owned())
            .unwrap_or_else(|_| "cmd.exe".to_string())
    }));
    let parameters = wide_null(&format!(
        "/d /v:on /s /c {}",
        quote_windows_argument(&script)
    ));
    let verb = wide_null("runas");
    // SAFETY: SHELLEXECUTEINFOW 是 POD 结构，全零是它的合法初始状态；
    // 随后逐字段赋值，cbSize 也在使用前设置为真实大小。
    let mut execute_info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    execute_info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    execute_info.fMask = SEE_MASK_NOCLOSEPROCESS;
    execute_info.lpVerb = verb.as_ptr();
    execute_info.lpFile = batch_program.as_ptr();
    execute_info.lpParameters = parameters.as_ptr();
    execute_info.nShow = 0;

    let cleanup = |paths: &[PathBuf]| {
        for path in paths {
            let _ = fs::remove_file(path);
        }
        let _ = fs::remove_file(&status_path);
    };

    // SAFETY: execute_info 已完成初始化且 cbSize/lpFile/lpVerb/lpParameters 都指向
    // 生命周期覆盖本次调用的 NUL 结尾宽字符串（`verb`/`batch_program`/`parameters`
    // 在函数结束前不会被移动或释放）。返回 0 表示失败，按 GetLastError 处理。
    let launched = unsafe { ShellExecuteExW(&mut execute_info) };
    if launched == 0 {
        let error = std::io::Error::last_os_error();
        cleanup(&log_paths);
        if error.raw_os_error() == Some(1223) {
            return Err(DomainError::UserCancelled(
                "已取消管理员授权，未安装驱动。".to_string(),
            ));
        }
        return Err(DomainError::ExternalTool(format!(
            "无法以管理员权限启动 pnputil：{error}"
        )));
    }

    let process = execute_info.hProcess;
    if process.is_null() {
        cleanup(&log_paths);
        return Err(DomainError::ExternalTool(
            "管理员驱动安装程序未返回进程句柄。".to_string(),
        ));
    }

    let wait_result = loop {
        // SAFETY: process 是 ShellExecuteExW 成功返回的有效进程句柄，
        // 直到 CloseHandle 之前都保持有效。
        match unsafe { WaitForSingleObject(process, Duration::from_millis(100).as_millis() as u32) }
        {
            WAIT_OBJECT_0 => break Ok(()),
            WAIT_TIMEOUT => continue,
            _ => {
                break Err(DomainError::ExternalTool(format!(
                    "等待 pnputil 结束失败：{}",
                    std::io::Error::last_os_error()
                )))
            }
        }
    };
    // SAFETY: process 有效且尚未关闭；关闭后不再使用该句柄。
    unsafe {
        CloseHandle(process);
    }
    if let Err(error) = wait_result {
        cleanup(&log_paths);
        return Err(error);
    }

    let status = fs::read_to_string(&status_path).unwrap_or_default();
    let exit_codes = parse_batch_exit_codes(&status, commands.len());
    let mut outputs = Vec::with_capacity(commands.len());
    for (index, log) in log_paths.iter().enumerate() {
        outputs.push(ProcessOutput {
            exit_code: exit_codes.get(index).copied().unwrap_or(-1),
            stdout: take_elevated_output_log(log),
            stderr: String::new(),
        });
    }
    let _ = fs::remove_file(&status_path);
    let _ = fs::remove_dir(&scratch);
    Ok(outputs)
}

/// 解析状态文件里的 `序号 退出码` 行。
#[cfg(windows)]
fn parse_batch_exit_codes(status: &str, expected: usize) -> Vec<i32> {
    let mut codes = vec![-1; expected];
    for line in status.lines() {
        let mut fields = line.split_whitespace();
        let (Some(index), Some(code)) = (fields.next(), fields.next()) else {
            continue;
        };
        if let (Ok(index), Ok(code)) = (index.parse::<usize>(), code.parse::<i32>()) {
            if index < expected {
                codes[index] = code;
            }
        }
    }
    codes
}

/// 提权进程可写的临时目录（用户 Temp 下的专用子目录）。
#[cfg(windows)]
fn elevated_scratch_directory() -> PathBuf {
    std::env::temp_dir().join("NWflash").join("elevated")
}

#[cfg(not(windows))]
fn run_elevated_process(_command: ProcessCommand) -> Result<ProcessOutput, DomainError> {
    Err(DomainError::ExternalTool(
        "USB 驱动安装仅支持 Windows。".to_string(),
    ))
}

#[cfg(not(windows))]
fn run_elevated_processes(_commands: &[ProcessCommand]) -> Result<Vec<ProcessOutput>, DomainError> {
    Err(DomainError::ExternalTool(
        "USB 驱动安装仅支持 Windows。".to_string(),
    ))
}

#[cfg(windows)]
fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
fn windows_command_line(args: &[String]) -> String {
    args.iter()
        .map(|argument| quote_windows_argument(argument))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(windows)]
fn quote_windows_argument(argument: &str) -> String {
    if !argument.contains([' ', '\t', '"']) {
        return argument.to_string();
    }

    let mut quoted = String::from("\"");
    let mut backslashes = 0;
    for character in argument.chars() {
        match character {
            '\\' => backslashes += 1,
            '"' => {
                quoted.push_str(&"\\".repeat(backslashes * 2 + 1));
                quoted.push('"');
                backslashes = 0;
            }
            _ => {
                quoted.push_str(&"\\".repeat(backslashes));
                quoted.push(character);
                backslashes = 0;
            }
        }
    }
    quoted.push_str(&"\\".repeat(backslashes * 2));
    quoted.push('"');
    quoted
}

#[cfg(test)]
mod merge_outcome_tests {
    use super::*;

    fn output(exit_code: i32) -> ProcessOutput {
        ProcessOutput {
            exit_code,
            stdout: String::new(),
            stderr: String::new(),
        }
    }

    /// pnputil 退出码 5（「已存在」）是重装场景的正常结果，必须按成功处理；
    /// 只有真实失败码（如用法错误 1）才让整体失败。此前 `== 0` 判定与
    /// 旧的 `!= 0` 判定对 5 的结论相同——重装已装好的驱动会被误报失败。
    #[test]
    fn already_present_exit_code_is_not_a_failure() {
        let all_present = merge_install_outcomes(&[output(5), output(5), output(0)]);
        assert!(
            driver_install_succeeded(&all_present),
            "全部「已存在」必须算安装成功：{all_present:?}"
        );

        let mixed = merge_install_outcomes(&[output(5), output(1), output(5)]);
        assert!(
            !driver_install_succeeded(&mixed),
            "混入真实失败码必须整体失败：{mixed:?}"
        );
        assert_eq!(mixed.exit_code, 1, "整体退出码取第一条真实失败命令的");
    }
}
