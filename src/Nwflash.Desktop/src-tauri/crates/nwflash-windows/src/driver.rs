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
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemElevatedProcessExecutor;

impl ElevatedProcessExecutor for SystemElevatedProcessExecutor {
    fn run_elevated(&self, command: ProcessCommand) -> Result<ProcessOutput, DomainError> {
        run_elevated_process(command)
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

            for inf in frozen.inf_paths.clone() {
                frozen.revalidate()?;
                let output = self
                    .executor
                    .run_elevated(build_pnputil_install_command(&inf)?)?;
                if output.exit_code != 0 {
                    return Ok(output.exit_code);
                }
            }
            // Modern adb has these VIDs built in, so preserving the successful driver
            // installation result is more important than this compatibility supplement.
            let _ = write_vivo_adb_usb_ids(&self.adb_usb_ini_path);
            Ok(0)
        })();
        drop(staging);
        let cleanup = fs::remove_dir_all(&staging_path);
        match (result, cleanup) {
            (Ok(_), Err(error)) => Err(DomainError::Internal(format!(
                "清理驱动临时目录失败：{error}"
            ))),
            (result, _) => result,
        }
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

fn build_pnputil_install_command(inf: &Path) -> Result<ProcessCommand, DomainError> {
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
    Ok(ProcessCommand::new(
        pnputil.to_string_lossy(),
        vec![
            "/add-driver".to_string(),
            inf.to_string_lossy().into_owned(),
            "/install".to_string(),
        ],
    ))
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

pub fn run_elevated_process_observed(
    command: ProcessCommand,
    observer: std::sync::Arc<dyn crate::process::ProcessOutputObserver>,
) -> crate::process::ObservedProcessOutcome {
    run_elevated_process_observed_with(command, observer, run_elevated_process)
}

fn run_elevated_process_observed_with<F>(
    command: ProcessCommand,
    observer: std::sync::Arc<dyn crate::process::ProcessOutputObserver>,
    execute: F,
) -> crate::process::ObservedProcessOutcome
where
    F: FnOnce(ProcessCommand) -> Result<ProcessOutput, DomainError>,
{
    let dispatcher = crate::process::ObservationDispatcher::new(observer);
    let validation = crate::process::validate_command(&command.program)
        .and_then(|_| crate::process::validate_args(&command.args))
        .and_then(|_| {
            if command.working_directory.is_some() || !command.environment.is_empty() {
                Err(DomainError::InvalidInput(
                    "驱动安装命令不支持工作目录或环境变量。".to_string(),
                ))
            } else {
                Ok(())
            }
        });
    if let Err(error) = validation {
        return dispatcher.outcome(Err(error));
    }

    dispatcher.started(&crate::process::ProcessStartMetadata {
        program: command.program.clone(),
        args: command.args.clone(),
        working_directory: None,
        stdin_mode: crate::process::ProcessStdinMode::Inherit,
        stdout_mode: crate::process::ProcessStdoutMode::Unavailable,
        elevated: true,
    });
    let result = execute(command);
    let finish = match &result {
        Ok(output) => crate::process::ProcessFinishMetadata {
            exit_code: Some(output.exit_code),
            termination: crate::process::ProcessTermination::Completed,
            process_tree_termination_requested: false,
        },
        Err(DomainError::UserCancelled(_)) => crate::process::ProcessFinishMetadata {
            exit_code: None,
            termination: crate::process::ProcessTermination::Cancelled,
            process_tree_termination_requested: false,
        },
        Err(_) => crate::process::ProcessFinishMetadata {
            exit_code: None,
            termination: crate::process::ProcessTermination::WaitFailed,
            process_tree_termination_requested: false,
        },
    };
    dispatcher.finished(finish);
    dispatcher.outcome(result)
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

#[cfg(test)]
mod observation_tests {
    use super::*;
    use crate::process::{
        ProcessFinishMetadata, ProcessObservation, ProcessObserverError, ProcessOutputObserver,
        ProcessStartMetadata, ProcessTermination,
    };
    use std::sync::{Arc, Mutex};

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Event {
        Started(ProcessStartMetadata),
        Output,
        Finished(ProcessFinishMetadata),
    }

    #[derive(Clone, Default)]
    struct Observer(Arc<Mutex<Vec<Event>>>);

    impl ProcessOutputObserver for Observer {
        fn observe(&self, observation: ProcessObservation<'_>) -> Result<(), ProcessObserverError> {
            let event = match observation {
                ProcessObservation::Started(metadata) => Event::Started(metadata.clone()),
                ProcessObservation::Output { .. } => Event::Output,
                ProcessObservation::Finished(metadata) => Event::Finished(metadata),
            };
            self.0.lock().expect("event lock should hold").push(event);
            Ok(())
        }
    }

    #[test]
    fn elevated_execution_observes_start_and_exit_without_fake_output() {
        let observer = Observer::default();
        let outcome = run_elevated_process_observed_with(
            ProcessCommand::new(
                r"C:\Windows\System32\pnputil.exe",
                ["/enum-drivers".to_string()],
            ),
            Arc::new(observer.clone()),
            |_| {
                Ok(ProcessOutput {
                    exit_code: 7,
                    stdout: String::new(),
                    stderr: String::new(),
                })
            },
        );

        assert_eq!(
            outcome.result.expect("fixture should complete").exit_code,
            7
        );
        let events = observer.0.lock().expect("event lock should hold").clone();
        assert!(matches!(
            events.first(),
            Some(Event::Started(ProcessStartMetadata { elevated: true, .. }))
        ));
        assert!(!events.contains(&Event::Output));
        assert!(matches!(
            events.last(),
            Some(Event::Finished(ProcessFinishMetadata {
                exit_code: Some(7),
                termination: ProcessTermination::Completed,
                process_tree_termination_requested: false,
            }))
        ));
    }
}
