//! Read-only Windows driver capability detection.

use std::{
    collections::HashSet,
    fs, io,
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use nwflash_domain::DomainError;

use crate::process::{ProcessCommand, ProcessOutput};

const ADB_MARKER: &str = "android_winusb";
const FASTBOOT_MARKER: &str = "android_usb";
const MEDIATEK_MARKER: &str = "cdc-acm";
const VIVO_ADB_IDS: [&str; 4] = ["0x2D95", "0x9BB5", "0x18D1", "0x0E8D"];
pub const BUNDLED_DRIVER_ARCHIVE_FILE_NAME: &str = "vivo-usb-driver.7z";
static DRIVER_STAGING_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub trait DriverArchiveExtractor: Send + Sync {
    fn extract(&self, archive: &Path, destination: &Path) -> Result<(), DomainError>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct BundledDriverArchiveExtractor;

impl DriverArchiveExtractor for BundledDriverArchiveExtractor {
    fn extract(&self, archive: &Path, destination: &Path) -> Result<(), DomainError> {
        extract_driver_archive(archive, destination)
    }
}

pub trait ElevatedProcessExecutor: Send + Sync {
    fn run_elevated(&self, command: ProcessCommand) -> Result<ProcessOutput, DomainError>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemElevatedProcessExecutor;

impl ElevatedProcessExecutor for SystemElevatedProcessExecutor {
    fn run_elevated(&self, command: ProcessCommand) -> Result<ProcessOutput, DomainError> {
        run_elevated_process(command)
    }
}

pub struct DriverInstaller<A = BundledDriverArchiveExtractor, E = SystemElevatedProcessExecutor> {
    archive_path: PathBuf,
    staging_root: PathBuf,
    adb_usb_ini_path: PathBuf,
    extractor: A,
    executor: E,
}

impl DriverInstaller {
    pub fn new(archive_path: PathBuf, adb_usb_ini_path: PathBuf) -> Self {
        Self::with_dependencies(
            archive_path,
            std::env::temp_dir().join("NWflash").join("drivers"),
            adb_usb_ini_path,
            BundledDriverArchiveExtractor,
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

impl<A, E> DriverInstaller<A, E>
where
    A: DriverArchiveExtractor,
    E: ElevatedProcessExecutor,
{
    pub fn with_dependencies(
        archive_path: PathBuf,
        staging_root: PathBuf,
        adb_usb_ini_path: PathBuf,
        extractor: A,
        executor: E,
    ) -> Self {
        Self {
            archive_path,
            staging_root,
            adb_usb_ini_path,
            extractor,
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
        let staging = self.create_staging_directory()?;
        let result = (|| {
            self.extractor.extract(&self.archive_path, &staging)?;
            if !contains_inf_recursively(&staging) {
                return Err(DomainError::InvalidFormat(
                    "驱动包内未找到任何 INF，请重新下载安装包。".to_string(),
                ));
            }
            if should_cancel() {
                return Err(DomainError::UserCancelled("用户取消驱动安装。".to_string()));
            }

            let output = self
                .executor
                .run_elevated(build_pnputil_install_command(&staging)?)?;
            if output.exit_code == 0 {
                // Modern adb has these VIDs built in, so preserving the successful driver
                // installation result is more important than this compatibility supplement.
                let _ = write_vivo_adb_usb_ids(&self.adb_usb_ini_path);
            }
            Ok(output.exit_code)
        })();
        let _ = fs::remove_dir_all(&staging);
        result
    }

    fn create_staging_directory(&self) -> Result<PathBuf, DomainError> {
        let sequence = DRIVER_STAGING_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let directory = self
            .staging_root
            .join(format!("{}-{}", std::process::id(), sequence,));
        fs::create_dir_all(&directory)
            .map_err(|error| DomainError::Internal(format!("创建驱动临时目录失败：{error}")))?;
        Ok(directory)
    }
}

#[derive(Debug, Clone)]
pub struct DriverDetectionPaths {
    driver_store_directories: Vec<PathBuf>,
    legacy_install_directories: Vec<PathBuf>,
}

impl DriverDetectionPaths {
    pub fn new(
        driver_store_directories: Vec<PathBuf>,
        legacy_install_directories: Vec<PathBuf>,
    ) -> Self {
        Self {
            driver_store_directories,
            legacy_install_directories,
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

        Self::new(vec![driver_store], legacy_install_directories)
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
    let legacy_installed = paths
        .legacy_install_directories
        .iter()
        .any(|path| contains_inf_recursively(path));

    DriverStatus {
        adb_installed: legacy_installed
            || has_driver_store_marker(&paths.driver_store_directories, ADB_MARKER),
        fastboot_installed: legacy_installed
            || has_driver_store_marker(&paths.driver_store_directories, FASTBOOT_MARKER),
        mediatek_installed: legacy_installed
            || has_mediatek_driver(&paths.driver_store_directories),
    }
}

pub fn build_pnputil_install_command(staging: &Path) -> Result<ProcessCommand, DomainError> {
    if staging.as_os_str().is_empty() {
        return Err(DomainError::InvalidInput(
            "驱动临时目录不能为空。".to_string(),
        ));
    }

    let windows = std::env::var_os("WINDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    Ok(ProcessCommand::new(
        windows
            .join("System32")
            .join("pnputil.exe")
            .to_string_lossy(),
        vec![
            "/add-driver".to_string(),
            staging.join("*.inf").to_string_lossy().into_owned(),
            "/subdirs".to_string(),
            "/install".to_string(),
        ],
    ))
}

/// Safely extracts the bundled vivo driver archive into a caller-owned staging directory.
///
/// `sevenz-rust`'s default callback joins entry names directly, so all archive paths are
/// validated here before any filesystem write happens.
pub fn extract_driver_archive(archive: &Path, destination: &Path) -> Result<(), DomainError> {
    let metadata = fs::metadata(archive).map_err(|_| {
        DomainError::InvalidInput("未找到内置 USB 驱动包，请重新安装奶蛙Flash。".to_string())
    })?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err(DomainError::InvalidFormat(
            "内置 USB 驱动包无效，请重新下载安装包。".to_string(),
        ));
    }

    fs::create_dir_all(destination)
        .map_err(|error| DomainError::Internal(format!("创建驱动临时目录失败：{error}")))?;
    let destination = destination
        .canonicalize()
        .map_err(|error| DomainError::Internal(format!("读取驱动临时目录失败：{error}")))?;

    sevenz_rust::decompress_file_with_extract_fn(archive, &destination, |entry, reader, _| {
        let Some(relative) = safe_archive_entry_path(entry.name(), entry.is_directory())? else {
            return Ok(true);
        };
        let target = destination.join(relative);

        if entry.is_anti_item() {
            return Err(sevenz_rust::Error::other(
                "anti-item entries are not allowed",
            ));
        }
        if entry.is_directory() {
            fs::create_dir_all(&target).map_err(sevenz_rust::Error::io)?;
        } else {
            let parent = target.parent().ok_or_else(|| {
                sevenz_rust::Error::other("driver archive file has no parent directory")
            })?;
            fs::create_dir_all(parent).map_err(sevenz_rust::Error::io)?;
            let mut output = fs::File::create(&target).map_err(sevenz_rust::Error::io)?;
            io::copy(reader, &mut output).map_err(sevenz_rust::Error::io)?;
        }

        Ok(true)
    })
    .map_err(|_| {
        DomainError::InvalidFormat("驱动包无法安全解压，请重新下载安装包。".to_string())
    })?;

    if !contains_inf_recursively(&destination) {
        return Err(DomainError::InvalidFormat(
            "驱动包内未找到任何 INF，请重新下载安装包。".to_string(),
        ));
    }

    Ok(())
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

#[cfg(windows)]
fn run_elevated_process(command: ProcessCommand) -> Result<ProcessOutput, DomainError> {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, WAIT_OBJECT_0, WAIT_TIMEOUT},
        System::Threading::{GetExitCodeProcess, WaitForSingleObject},
        UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW},
    };

    crate::process::validate_command(&command.program)?;
    crate::process::validate_args(&command.args)?;
    if command.working_directory.is_some() || !command.environment.is_empty() {
        return Err(DomainError::InvalidInput(
            "驱动安装命令不支持工作目录或环境变量。".to_string(),
        ));
    }

    let verb = wide_null("runas");
    let program = wide_null(&command.program);
    let parameters = wide_null(&windows_command_line(&command.args));
    let mut execute_info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    execute_info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    execute_info.fMask = SEE_MASK_NOCLOSEPROCESS;
    execute_info.lpVerb = verb.as_ptr();
    execute_info.lpFile = program.as_ptr();
    execute_info.lpParameters = parameters.as_ptr();
    execute_info.nShow = 0;

    let launched = unsafe { ShellExecuteExW(&mut execute_info) };
    if launched == 0 {
        let error = std::io::Error::last_os_error();
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
        return Err(DomainError::ExternalTool(
            "管理员驱动安装程序未返回进程句柄。".to_string(),
        ));
    }

    let result = loop {
        match unsafe { WaitForSingleObject(process, Duration::from_millis(100).as_millis() as u32) }
        {
            WAIT_OBJECT_0 => {
                let mut exit_code = 0_u32;
                if unsafe { GetExitCodeProcess(process, &mut exit_code) } == 0 {
                    break Err(DomainError::ExternalTool(format!(
                        "读取 pnputil 退出码失败：{}",
                        std::io::Error::last_os_error()
                    )));
                }
                break Ok(ProcessOutput {
                    exit_code: exit_code as i32,
                    stdout: String::new(),
                    stderr: String::new(),
                });
            }
            WAIT_TIMEOUT => continue,
            _ => {
                break Err(DomainError::ExternalTool(format!(
                    "等待 pnputil 结束失败：{}",
                    std::io::Error::last_os_error()
                )))
            }
        }
    };
    unsafe {
        CloseHandle(process);
    }
    result
}

#[cfg(not(windows))]
fn run_elevated_process(_command: ProcessCommand) -> Result<ProcessOutput, DomainError> {
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
