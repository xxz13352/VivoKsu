use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use zip::ZipArchive;

use crate::{
    paths::resource_root,
    remote_assets::RemoteAssetSpec,
    resource_downloader::{ProgressSink, RemoteAssetDownloader, ResourceDownloadError},
};

const SCRCPY_ARCHIVE_NAME: &str = "scrcpy-win64-v4.1.zip";
const SCRCPY_RELEASE_URL: &str =
    "https://github.com/Genymobile/scrcpy/releases/download/v4.1/scrcpy-win64-v4.1.zip";
const SCRCPY_ARCHIVE_SHA256: &str =
    "5b12172b3264b2889f4583ee64752ce832e29bc8b1089dca81093459697165db";
const SCRCPY_ARCHIVE_SIZE: u64 = 11_305_298;
const SCRCPY_EXECUTABLE_NAME: &str = "scrcpy.exe";
const SCRCPY_MANIFEST_NAME: &str = "scrcpy-files.sha256";

#[derive(Debug, Error)]
pub enum ScrcpyProvisionError {
    #[error("无效的 scrcpy 资产 URL。")]
    InvalidAssetUrl,
    #[error("scrcpy 发布资产缺少有效的 SHA-256 摘要。")]
    InvalidAssetDigest,
    #[error("scrcpy 发布资产大小无效。")]
    InvalidAssetSize,
    #[error("未找到可安装的 scrcpy.exe。")]
    ScrcpyExecutableMissing,
    #[error("文件名异常：{0}")]
    InvalidFilename(String),
    #[error("归档内容不安全：{0}")]
    UnsafeArchive(String),
    #[error("scrcpy 安装完整性校验失败：{0}")]
    Integrity(String),
    #[error("下载失败: {0}")]
    Download(#[from] ResourceDownloadError),
    #[error("内置 scrcpy 资源缺失或校验失败，请重新安装应用。")]
    BundledResourceMissing,
    #[error("IO: {0}")]
    Io(String),
    #[error("ZIP 解析失败: {0}")]
    Zip(String),
}

#[derive(Debug)]
pub struct ScrcpyProvisioner {
    downloader: Option<RemoteAssetDownloader>,
    installation_root: PathBuf,
    bundled_root: Option<PathBuf>,
    provisioning_lock: tokio::sync::Mutex<()>,
}

impl ScrcpyProvisioner {
    pub fn new() -> Self {
        Self::with_options(
            Some(RemoteAssetDownloader::default()),
            resource_root().join("scrcpy"),
            None,
        )
    }

    pub fn bundled(bundle_root: PathBuf) -> Self {
        Self::with_options(
            None,
            resource_root().join("scrcpy"),
            Some(bundle_root.join("scrcpy")),
        )
    }

    fn with_options(
        downloader: Option<RemoteAssetDownloader>,
        installation_root: PathBuf,
        bundled_root: Option<PathBuf>,
    ) -> Self {
        Self {
            downloader,
            installation_root,
            bundled_root,
            provisioning_lock: tokio::sync::Mutex::new(()),
        }
    }

    pub fn installation_root(&self) -> &Path {
        &self.installation_root
    }

    pub fn is_installed(&self) -> bool {
        self.find_installed_executable().is_some()
    }

    pub fn installed_executable(&self) -> Option<PathBuf> {
        self.find_installed_executable()
    }

    pub async fn ensure_installed(
        &self,
        cancellation_token: &CancellationToken,
        progress: Option<&ProgressSink>,
    ) -> Result<PathBuf, ScrcpyProvisionError> {
        if let Some(existing) = self.find_installed_executable() {
            return Ok(existing);
        }

        let _guard = self.provisioning_lock.lock().await;
        self.cleanup_stale_staging_directories();
        if let Some(existing) = self.find_installed_executable() {
            return Ok(existing);
        }

        let asset_spec = pinned_scrcpy_asset();

        let staging_root = self
            .installation_root
            .join(format!(".staging-{}", unique_suffix()));
        let (archive_path, package_name) =
            staging_archive_path(&staging_root, SCRCPY_ARCHIVE_NAME)?;
        let payload_root = staging_root.join("payload");
        let package_root = self.installation_root.join(&package_name);

        let provision_result = self
            .provision_from_remote(
                &asset_spec,
                &archive_path,
                &payload_root,
                progress,
                cancellation_token,
            )
            .await
            .and_then(|()| {
                fs::create_dir_all(&self.installation_root)
                    .map_err(|error| ScrcpyProvisionError::Io(error.to_string()))?;
                let candidate_root = staging_root.join("package");
                publish_payload(&payload_root, &candidate_root)?;
                let executable = self.resolve_package_executable(&candidate_root)?;
                write_published_manifest(&executable)?;
                verify_published_package(&candidate_root)?;
                let relative_executable = executable
                    .strip_prefix(&candidate_root)
                    .map_err(|error| ScrcpyProvisionError::Io(error.to_string()))?
                    .to_path_buf();
                replace_published_package(&candidate_root, &package_root)?;
                Ok(package_root.join(relative_executable))
            });

        let _ = self.try_remove_dir_all(&staging_root);
        provision_result
    }

    async fn provision_from_remote(
        &self,
        asset_spec: &RemoteAssetSpec,
        archive_path: &Path,
        payload_root: &Path,
        progress: Option<&ProgressSink>,
        cancellation_token: &CancellationToken,
    ) -> Result<(), ScrcpyProvisionError> {
        validate_pinned_scrcpy_asset(asset_spec)?;

        self.downloader
            .as_ref()
            .ok_or(ScrcpyProvisionError::BundledResourceMissing)?
            .download_to_file(asset_spec, archive_path, progress, cancellation_token)
            .await?;

        extract_archive_safely(archive_path, payload_root)?;
        if Self::find_executable(payload_root, false).is_none() {
            return Err(ScrcpyProvisionError::ScrcpyExecutableMissing);
        }

        Ok(())
    }

    fn find_installed_executable(&self) -> Option<PathBuf> {
        self.find_bundled_executable()
            .or_else(|| self.find_verified_executable(&self.installation_root, true))
    }

    fn find_bundled_executable(&self) -> Option<PathBuf> {
        if let Some(root) = &self.bundled_root {
            return self.find_verified_executable(root, false);
        }
        let root = std::env::current_exe()
            .ok()?
            .parent()?
            .join("resources")
            .join("scrcpy");
        self.find_verified_executable(&root, false)
    }

    fn find_verified_executable(&self, root: &Path, skip_staging: bool) -> Option<PathBuf> {
        if !root.exists() {
            return None;
        }

        let mut stack = vec![root.to_path_buf()];
        while let Some(current) = stack.pop() {
            let entries = fs::read_dir(&current).ok()?.collect::<Vec<_>>();
            for entry in entries.into_iter().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    if skip_staging && is_staging_directory(&path) {
                        continue;
                    }
                    stack.push(path);
                    continue;
                }

                if path.file_name().and_then(|name| name.to_str()) == Some(SCRCPY_EXECUTABLE_NAME)
                    && verify_published_package_from_executable(&path).is_ok()
                {
                    return Some(path);
                }
            }
        }

        None
    }

    fn find_executable(root: &Path, skip_staging: bool) -> Option<PathBuf> {
        if !root.exists() {
            return None;
        }

        let mut stack = vec![root.to_path_buf()];
        while let Some(current) = stack.pop() {
            let mut entries = fs::read_dir(&current).ok()?.collect::<Vec<_>>();
            for entry in entries.drain(..) {
                let entry = entry.ok()?;
                let path = entry.path();
                if path.is_dir() {
                    if skip_staging && is_staging_directory(&path) {
                        continue;
                    }

                    stack.push(path);
                    continue;
                }

                if path.file_name().and_then(|name| name.to_str()) == Some(SCRCPY_EXECUTABLE_NAME)
                    && is_non_empty_file(&path)
                {
                    return Some(path);
                }
            }
        }

        None
    }

    fn resolve_package_executable(
        &self,
        package_root: &Path,
    ) -> Result<PathBuf, ScrcpyProvisionError> {
        Self::find_executable(package_root, false)
            .ok_or(ScrcpyProvisionError::ScrcpyExecutableMissing)
    }

    pub(crate) fn cleanup_stale_staging_directories(&self) {
        if !self.installation_root.exists() {
            return;
        }

        for path in Self::find_staging_directories(&self.installation_root) {
            let _ = self.try_remove_dir_all(&path);
        }
    }

    fn find_staging_directories(root: &Path) -> Vec<PathBuf> {
        let Ok(entries) = fs::read_dir(root) else {
            return Vec::new();
        };

        entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_dir() && is_staging_directory(path))
            .collect()
    }

    fn try_remove_dir_all(&self, path: &Path) -> std::io::Result<()> {
        if path.exists() {
            fs::remove_dir_all(path)?;
        }
        Ok(())
    }
}

impl Default for ScrcpyProvisioner {
    fn default() -> Self {
        Self::new()
    }
}

fn is_staging_directory(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with(".staging-"))
}

fn unique_suffix() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos())
}

fn is_non_empty_file(path: &Path) -> bool {
    fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.len() > 0)
        .unwrap_or(false)
}

fn package_name_from_asset(name: &str) -> Result<String, ScrcpyProvisionError> {
    let path = Path::new(name);
    if name.is_empty()
        || path.is_absolute()
        || name.contains('/')
        || name.contains('\\')
        || name.contains("..")
    {
        return Err(ScrcpyProvisionError::InvalidFilename(name.to_string()));
    }

    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| *value == name)
        .ok_or_else(|| ScrcpyProvisionError::InvalidFilename(name.to_string()))?;
    let package_name = Path::new(file_name)
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ScrcpyProvisionError::InvalidFilename(name.to_string()))?;
    Ok(package_name.to_string())
}

fn staging_archive_path(
    staging_root: &Path,
    asset_name: &str,
) -> Result<(PathBuf, String), ScrcpyProvisionError> {
    let package_name = package_name_from_asset(asset_name)?;
    Ok((staging_root.join(asset_name), package_name))
}

fn compute_sha256(path: &Path) -> Result<String, ScrcpyProvisionError> {
    let mut file = File::open(path).map_err(|error| ScrcpyProvisionError::Io(error.to_string()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 8192];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| ScrcpyProvisionError::Io(error.to_string()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn write_published_manifest(executable: &Path) -> Result<(), ScrcpyProvisionError> {
    let package_root = executable
        .parent()
        .ok_or_else(|| ScrcpyProvisionError::Integrity("scrcpy 包目录无效".to_string()))?;
    let entries = collect_package_hashes(package_root)?;
    let manifest = package_root.join(SCRCPY_MANIFEST_NAME);
    let content = entries
        .iter()
        .map(|(relative_path, hash)| format!("{hash} *{relative_path}"))
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(manifest, format!("{content}\n"))
        .map_err(|error| ScrcpyProvisionError::Io(error.to_string()))
}

fn verify_published_package_from_executable(executable: &Path) -> Result<(), ScrcpyProvisionError> {
    if !is_non_empty_file(executable) {
        return Err(ScrcpyProvisionError::ScrcpyExecutableMissing);
    }

    let package_root = executable
        .parent()
        .ok_or_else(|| ScrcpyProvisionError::Integrity("scrcpy 包目录无效".to_string()))?;
    let manifest = package_root.join(SCRCPY_MANIFEST_NAME);
    let expected = fs::read_to_string(manifest)
        .map_err(|error| ScrcpyProvisionError::Io(error.to_string()))?;
    let expected = parse_package_manifest(&expected)?;
    let actual = collect_package_hashes(package_root)?;
    let executable_name = executable
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| ScrcpyProvisionError::Integrity("scrcpy 可执行文件名无效".to_string()))?;
    if executable_name != SCRCPY_EXECUTABLE_NAME
        || !expected.contains_key(SCRCPY_EXECUTABLE_NAME)
        || expected.len() != actual.len()
        || expected
            .iter()
            .any(|(relative_path, hash)| actual.get(relative_path) != Some(hash))
    {
        return Err(ScrcpyProvisionError::Integrity(
            "scrcpy 包完整性校验失败".to_string(),
        ));
    }
    Ok(())
}

fn verify_published_package(package_root: &Path) -> Result<(), ScrcpyProvisionError> {
    let executable = ScrcpyProvisioner::find_executable(package_root, false)
        .ok_or(ScrcpyProvisionError::ScrcpyExecutableMissing)?;
    verify_published_package_from_executable(&executable)
}

fn collect_package_hashes(
    package_root: &Path,
) -> Result<BTreeMap<String, String>, ScrcpyProvisionError> {
    if !package_root.is_dir() {
        return Err(ScrcpyProvisionError::Integrity(
            "scrcpy 包目录不存在".to_string(),
        ));
    }

    let manifest_path = package_root.join(SCRCPY_MANIFEST_NAME);
    let mut entries = BTreeMap::new();
    let mut stack = vec![package_root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        for entry in
            fs::read_dir(&directory).map_err(|error| ScrcpyProvisionError::Io(error.to_string()))?
        {
            let entry = entry.map_err(|error| ScrcpyProvisionError::Io(error.to_string()))?;
            let path = entry.path();
            let file_type = entry
                .file_type()
                .map_err(|error| ScrcpyProvisionError::Io(error.to_string()))?;
            if file_type.is_dir() {
                stack.push(path);
                continue;
            }
            if !file_type.is_file() {
                return Err(ScrcpyProvisionError::Integrity(
                    "scrcpy 包包含不受支持的文件类型".to_string(),
                ));
            }
            if path == manifest_path {
                continue;
            }
            if !is_non_empty_file(&path) {
                return Err(ScrcpyProvisionError::Integrity(
                    "scrcpy 包包含空文件".to_string(),
                ));
            }

            let relative_path = package_relative_path(package_root, &path)?;
            let hash = compute_sha256(&path)?;
            if entries.insert(relative_path, hash).is_some() {
                return Err(ScrcpyProvisionError::Integrity(
                    "scrcpy 包完整性清单重复".to_string(),
                ));
            }
        }
    }

    if entries.is_empty() {
        return Err(ScrcpyProvisionError::Integrity(
            "scrcpy 包不包含可校验文件".to_string(),
        ));
    }
    Ok(entries)
}

fn parse_package_manifest(
    manifest: &str,
) -> Result<BTreeMap<String, String>, ScrcpyProvisionError> {
    let mut entries = BTreeMap::new();
    for line in manifest
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let Some((hash, relative_path)) = line.split_once(" *") else {
            return Err(ScrcpyProvisionError::Integrity(
                "scrcpy 完整性清单无效".to_string(),
            ));
        };
        if hash.len() != 64
            || !hash.chars().all(|character| character.is_ascii_hexdigit())
            || !is_safe_package_relative_path(relative_path)
            || entries
                .insert(relative_path.to_string(), hash.to_ascii_lowercase())
                .is_some()
        {
            return Err(ScrcpyProvisionError::Integrity(
                "scrcpy 完整性清单无效".to_string(),
            ));
        }
    }
    if entries.is_empty() {
        return Err(ScrcpyProvisionError::Integrity(
            "scrcpy 完整性清单无效".to_string(),
        ));
    }
    Ok(entries)
}

fn package_relative_path(package_root: &Path, path: &Path) -> Result<String, ScrcpyProvisionError> {
    let relative_path = path
        .strip_prefix(package_root)
        .map_err(|error| ScrcpyProvisionError::Io(error.to_string()))?
        .to_string_lossy()
        .replace('\\', "/");
    if !is_safe_package_relative_path(&relative_path) {
        return Err(ScrcpyProvisionError::Integrity(
            "scrcpy 包路径无效".to_string(),
        ));
    }
    Ok(relative_path)
}

fn is_safe_package_relative_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.contains(':')
        && path
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

fn extract_archive_safely(
    archive_path: &Path,
    destination_root: &Path,
) -> Result<(), ScrcpyProvisionError> {
    let file =
        File::open(archive_path).map_err(|error| ScrcpyProvisionError::Io(error.to_string()))?;
    let mut archive =
        ZipArchive::new(file).map_err(|error| ScrcpyProvisionError::Zip(error.to_string()))?;

    fs::create_dir_all(destination_root)
        .map_err(|error| ScrcpyProvisionError::Io(error.to_string()))?;

    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| ScrcpyProvisionError::Zip(error.to_string()))?;
        let name = entry.name().to_string();

        if name.is_empty() {
            continue;
        }

        if has_unsafe_entry_name(&name) {
            return Err(ScrcpyProvisionError::UnsafeArchive(format!(
                "异常路径: {name}"
            )));
        }

        let destination = destination_root.join(name.replace('\\', "/"));
        if !destination.starts_with(destination_root) {
            return Err(ScrcpyProvisionError::UnsafeArchive(format!(
                "路径越界: {name}"
            )));
        }

        if name.ends_with('/') {
            fs::create_dir_all(&destination).map_err(|error| {
                ScrcpyProvisionError::Io(format!("创建目录失败 {name}: {error}"))
            })?;
            continue;
        }

        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                ScrcpyProvisionError::Io(format!("创建父目录失败 {name}: {error}"))
            })?;
        }

        let mut output = File::create(&destination)
            .map_err(|error| ScrcpyProvisionError::Io(error.to_string()))?;
        std::io::copy(&mut entry, &mut output).map_err(|error| {
            ScrcpyProvisionError::Io(format!("解压 zip 条目失败 {name}: {error}"))
        })?;
    }

    Ok(())
}

fn has_unsafe_entry_name(name: &str) -> bool {
    if name.starts_with('/') {
        return true;
    }
    if name.contains("..") || name.contains(':') {
        return true;
    }
    name.split('/').any(|segment| segment == "..")
}

fn publish_payload(source: &Path, destination: &Path) -> Result<(), ScrcpyProvisionError> {
    fs::create_dir_all(destination).map_err(|error| ScrcpyProvisionError::Io(error.to_string()))?;

    for entry in
        fs::read_dir(source).map_err(|error| ScrcpyProvisionError::Io(error.to_string()))?
    {
        let entry = entry.map_err(|error| ScrcpyProvisionError::Io(error.to_string()))?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());

        if source_path.is_dir() {
            publish_payload(&source_path, &destination_path)?;
            continue;
        }

        fs::copy(&source_path, &destination_path)
            .map_err(|error| ScrcpyProvisionError::Io(error.to_string()))?;
    }

    Ok(())
}

fn replace_published_package(
    candidate: &Path,
    destination: &Path,
) -> Result<(), ScrcpyProvisionError> {
    if !candidate.is_dir() {
        return Err(ScrcpyProvisionError::Io(format!(
            "scrcpy 候选包不存在：{}",
            candidate.display()
        )));
    }

    let destination_name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| ScrcpyProvisionError::Io("scrcpy 包路径无效。".to_string()))?;
    let backup =
        destination.with_file_name(format!(".{destination_name}.backup-{}", unique_suffix()));

    if destination.exists() {
        fs::rename(destination, &backup)
            .map_err(|error| ScrcpyProvisionError::Io(error.to_string()))?;
    }

    if let Err(error) = fs::rename(candidate, destination) {
        if backup.exists() {
            let _ = fs::rename(&backup, destination);
        }
        return Err(ScrcpyProvisionError::Io(error.to_string()));
    }

    if backup.exists() {
        fs::remove_dir_all(&backup).map_err(|error| ScrcpyProvisionError::Io(error.to_string()))?;
    }
    Ok(())
}

fn pinned_scrcpy_asset() -> RemoteAssetSpec {
    RemoteAssetSpec::new("scrcpy", SCRCPY_RELEASE_URL)
        .with_expected_sha256(SCRCPY_ARCHIVE_SHA256)
        .with_expected_length(SCRCPY_ARCHIVE_SIZE)
}

fn validate_pinned_scrcpy_asset(asset: &RemoteAssetSpec) -> Result<(), ScrcpyProvisionError> {
    if asset.github_url != SCRCPY_RELEASE_URL {
        return Err(ScrcpyProvisionError::InvalidAssetUrl);
    }
    if asset.expected_sha256.as_deref() != Some(SCRCPY_ARCHIVE_SHA256) {
        return Err(ScrcpyProvisionError::InvalidAssetDigest);
    }
    if asset.expected_length != Some(SCRCPY_ARCHIVE_SIZE) {
        return Err(ScrcpyProvisionError::InvalidAssetSize);
    }
    Ok(())
}

#[cfg(test)]
#[derive(Debug)]
struct GitHubReleaseAsset {
    name: String,
    browser_download_url: String,
    digest: Option<String>,
    size: u64,
}

#[cfg(test)]
fn asset_spec(asset: &GitHubReleaseAsset) -> Result<RemoteAssetSpec, ScrcpyProvisionError> {
    let digest = asset
        .digest
        .as_deref()
        .and_then(|value| value.strip_prefix("sha256:"))
        .filter(|value| {
            value.len() == 64 && value.chars().all(|character| character.is_ascii_hexdigit())
        })
        .ok_or(ScrcpyProvisionError::InvalidAssetDigest)?;
    let spec = RemoteAssetSpec::new("scrcpy", asset.browser_download_url.clone())
        .with_expected_sha256(digest)
        .with_expected_length(asset.size);
    if asset.name != SCRCPY_ARCHIVE_NAME {
        return Err(ScrcpyProvisionError::InvalidFilename(asset.name.clone()));
    }
    validate_pinned_scrcpy_asset(&spec)?;
    Ok(spec)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release_asset(digest: Option<&str>) -> GitHubReleaseAsset {
        GitHubReleaseAsset {
            name: "scrcpy-win64-v4.1.zip".to_string(),
            browser_download_url:
                "https://github.com/Genymobile/scrcpy/releases/download/v4.1/scrcpy-win64-v4.1.zip"
                    .to_string(),
            digest: digest.map(str::to_string),
            size: 11_305_298,
        }
    }

    #[test]
    fn scrcpy_release_asset_requires_a_well_formed_sha256_digest() {
        assert!(asset_spec(&release_asset(None)).is_err());
        assert!(asset_spec(&release_asset(Some("sha1:0123"))).is_err());
        assert!(asset_spec(&release_asset(Some("sha256:not-hex"))).is_err());
    }

    #[test]
    fn scrcpy_release_asset_pins_sha256_and_size() {
        let spec = asset_spec(&release_asset(Some(
            "sha256:5b12172b3264b2889f4583ee64752ce832e29bc8b1089dca81093459697165db",
        )))
        .expect("complete GitHub asset metadata should form a download specification");

        assert_eq!(
            spec.expected_sha256.as_deref(),
            Some("5b12172b3264b2889f4583ee64752ce832e29bc8b1089dca81093459697165db")
        );
        assert_eq!(spec.expected_length, Some(11_305_298));
    }

    #[test]
    fn scrcpy_release_asset_rejects_an_untrusted_download_host() {
        let mut asset = release_asset(Some(
            "sha256:5b12172b3264b2889f4583ee64752ce832e29bc8b1089dca81093459697165db",
        ));
        asset.browser_download_url = "https://untrusted.example/scrcpy-win64-v4.1.zip".to_string();

        assert!(asset_spec(&asset).is_err());
    }

    #[test]
    fn scrcpy_asset_names_must_be_safe_single_filenames() {
        assert!(package_name_from_asset("..\\scrcpy-win64-v4.1.zip").is_err());
        assert!(package_name_from_asset("C:\\scrcpy-win64-v4.1.zip").is_err());
        assert!(package_name_from_asset("scrcpy/../../payload.zip").is_err());
        assert_eq!(
            package_name_from_asset("scrcpy-win64-v4.1.zip")
                .expect("normal release asset name should be accepted"),
            "scrcpy-win64-v4.1"
        );
    }

    #[test]
    fn scrcpy_staging_archive_path_rejects_an_asset_path_before_joining_it() {
        let staging_root = std::env::temp_dir().join("nwflash-scrcpy-staging");
        let error = staging_archive_path(&staging_root, "scrcpy-win64-v4.1/../../outside.zip")
            .expect_err("an asset path must be rejected before it becomes a local path");

        assert!(matches!(error, ScrcpyProvisionError::InvalidFilename(_)));
        assert!(!staging_root.join("..\\outside.zip").exists());
    }

    #[test]
    fn scrcpy_installed_package_requires_a_matching_manifest() {
        let root =
            std::env::temp_dir().join(format!("nwflash-scrcpy-installed-{}", unique_suffix()));
        let executable = root.join("scrcpy.exe");
        std::fs::create_dir_all(&root).expect("package root should exist");
        std::fs::write(&executable, b"scrcpy fixture").expect("executable fixture should exist");

        assert!(verify_published_package(&root).is_err());

        std::fs::write(root.join(SCRCPY_MANIFEST_NAME), "not-the-file-hash\n")
            .expect("invalid manifest should exist");
        assert!(verify_published_package(&root).is_err());

        std::fs::remove_dir_all(root).expect("package root should be removed");
    }

    #[test]
    fn scrcpy_installed_package_accepts_a_matching_manifest() {
        let root = std::env::temp_dir().join(format!("nwflash-scrcpy-valid-{}", unique_suffix()));
        let executable = root.join("scrcpy.exe");
        std::fs::create_dir_all(&root).expect("package root should exist");
        std::fs::write(&executable, b"scrcpy fixture").expect("executable fixture should exist");
        write_published_manifest(&executable).expect("manifest should be written");

        assert!(verify_published_package(&root).is_ok());

        std::fs::remove_dir_all(root).expect("temporary package should be removed");
    }

    #[test]
    fn bundled_scrcpy_provisioner_uses_the_explicit_resource_tree() {
        let root = std::env::temp_dir().join(format!("nwflash-scrcpy-bundle-{}", unique_suffix()));
        let package = root.join("scrcpy");
        let executable = package.join("scrcpy.exe");
        std::fs::create_dir_all(&package).expect("bundle package should exist");
        std::fs::write(&executable, b"scrcpy fixture").expect("bundle executable should exist");
        write_published_manifest(&executable).expect("bundle manifest should be written");

        let provisioner = ScrcpyProvisioner::bundled(root.clone());

        assert!(provisioner.downloader.is_none());
        assert_eq!(provisioner.installed_executable(), Some(executable));
        std::fs::remove_dir_all(root).expect("bundle fixture should be removed");
    }

    #[test]
    fn scrcpy_installed_package_rejects_a_modified_runtime_dll() {
        let root = std::env::temp_dir().join(format!("nwflash-scrcpy-dll-{}", unique_suffix()));
        let executable = root.join("scrcpy.exe");
        let runtime_dll = root.join("SDL2.dll");
        std::fs::create_dir_all(&root).expect("package root should exist");
        std::fs::write(&executable, b"scrcpy fixture").expect("executable fixture should exist");
        std::fs::write(&runtime_dll, b"approved runtime").expect("runtime fixture should exist");
        write_published_manifest(&executable).expect("package manifest should be written");
        std::fs::write(&runtime_dll, b"modified runtime")
            .expect("runtime fixture should be modified");

        assert!(verify_published_package(&root).is_err());

        std::fs::remove_dir_all(root).expect("temporary package should be removed");
    }

    #[test]
    fn scrcpy_publish_staged_payload_survives_staging_cleanup() {
        let root = std::env::temp_dir().join(format!("nwflash-scrcpy-publish-{}", unique_suffix()));
        let staging_root = root.join(".staging-test");
        let payload_root = staging_root.join("payload");
        let package_root = root.join("scrcpy-win64-v4.1");
        std::fs::create_dir_all(&payload_root).expect("staged payload directory should exist");
        std::fs::write(payload_root.join("scrcpy.exe"), b"scrcpy fixture")
            .expect("staged executable should be written");

        publish_payload(&payload_root, &package_root)
            .expect("staged payload should be published before cleanup");
        std::fs::remove_dir_all(&staging_root).expect("staging should be removable after publish");

        assert_eq!(
            std::fs::read(package_root.join("scrcpy.exe"))
                .expect("published executable should survive cleanup"),
            b"scrcpy fixture"
        );
        std::fs::remove_dir_all(root).expect("test root should be removed");
    }

    #[test]
    fn scrcpy_failed_candidate_does_not_delete_the_previous_package() {
        let root = std::env::temp_dir().join(format!(
            "nwflash-scrcpy-package-rollback-{}",
            unique_suffix()
        ));
        let candidate = root.join(".staging").join("package");
        let destination = root.join("scrcpy-win64-v4.1");
        std::fs::create_dir_all(&destination).expect("existing package should exist");
        std::fs::write(destination.join("scrcpy.exe"), b"approved")
            .expect("existing executable should be written");

        let error = replace_published_package(&candidate, &destination)
            .expect_err("a missing candidate must fail without replacing the package");

        assert!(error.to_string().contains("候选包不存在"));
        assert_eq!(
            std::fs::read(destination.join("scrcpy.exe"))
                .expect("previous package must remain readable"),
            b"approved"
        );
        std::fs::remove_dir_all(root).expect("test root should be removed");
    }
}
