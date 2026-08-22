use std::{
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::{
    paths::resource_root,
    remote_assets::{
        github_download_url, PAYLOAD_DUMPER_ASSET_NAME, PAYLOAD_DUMPER_EXECUTABLE_NAME,
        PAYLOAD_DUMPER_SHA256,
    },
    resource_downloader::{ProgressSink, RemoteAssetDownloader, ResourceDownloadError},
};
use zip::ZipArchive;

#[derive(Debug, Error)]
pub enum PayloadProvisionError {
    #[error("Payload dumper 缺失且未配置下载器。")]
    MissingDownloader,
    #[error("下载失败: {0}")]
    Download(#[from] ResourceDownloadError),
    #[error("ZIP 解析失败: {0}")]
    Zip(String),
    #[error("payload_dumper.exe 不存在于压缩包。")]
    ExecutableMissing,
    #[error("文件缺失: {0}")]
    MissingFile(String),
    #[error("IO: {0}")]
    Io(String),
    #[error("完整性校验失败: {0}")]
    Integrity(String),
}

#[derive(Debug)]
pub struct PayloadDumperProvisioner {
    downloader: Option<RemoteAssetDownloader>,
    installation_root: PathBuf,
    bundled_executable_path: Option<PathBuf>,
    expected_sha256: String,
    provisioning_lock: tokio::sync::Mutex<()>,
}

impl PayloadDumperProvisioner {
    pub fn bundled(resource_root: PathBuf) -> Self {
        Self {
            downloader: None,
            installation_root: resource_root.join("payload-dumper-cache"),
            bundled_executable_path: Some(
                resource_root
                    .join("payload-tools")
                    .join(PAYLOAD_DUMPER_EXECUTABLE_NAME),
            ),
            expected_sha256: PAYLOAD_DUMPER_SHA256.to_string(),
            provisioning_lock: tokio::sync::Mutex::new(()),
        }
    }

    pub fn new(
        downloader: RemoteAssetDownloader,
        installation_root: Option<PathBuf>,
        bundled_executable_path: Option<PathBuf>,
    ) -> Self {
        Self::with_expected_sha256(
            downloader,
            installation_root,
            bundled_executable_path,
            PAYLOAD_DUMPER_SHA256,
        )
    }

    pub fn with_expected_sha256(
        downloader: RemoteAssetDownloader,
        installation_root: Option<PathBuf>,
        bundled_executable_path: Option<PathBuf>,
        expected_sha256: impl Into<String>,
    ) -> Self {
        Self {
            downloader: Some(downloader),
            installation_root: installation_root
                .unwrap_or_else(|| resource_root().join("payload-dumper")),
            bundled_executable_path,
            expected_sha256: expected_sha256.into(),
            provisioning_lock: tokio::sync::Mutex::new(()),
        }
    }

    pub fn installation_root(&self) -> &Path {
        &self.installation_root
    }

    pub fn bundled_executable_path(&self) -> Option<&Path> {
        self.bundled_executable_path.as_deref()
    }

    pub fn cached_executable_path(&self) -> PathBuf {
        self.installation_root.join(PAYLOAD_DUMPER_EXECUTABLE_NAME)
    }

    pub fn executable_path(&self) -> PathBuf {
        self.bundled_executable_path()
            .filter(|path| self.verify_executable_hash(path).is_ok())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.cached_executable_path())
    }

    pub fn is_available(&self) -> bool {
        if let Some(bundled) = &self.bundled_executable_path {
            if self.verify_executable_hash(bundled).is_ok() {
                return true;
            }
        }

        self.verify_executable_hash(&self.cached_executable_path())
            .is_ok()
    }

    pub async fn ensure_installed(
        &self,
        cancellation_token: &CancellationToken,
        progress: Option<&ProgressSink>,
    ) -> Result<PathBuf, PayloadProvisionError> {
        if let Some(bundled) = &self.bundled_executable_path {
            if self.verify_executable_hash(bundled).is_ok() {
                return Ok(bundled.clone());
            }
        }

        let cached = self.cached_executable_path();
        if cached.exists() && !self.discard_invalid_cached_executable(&cached)? {
            return Ok(cached);
        }

        let _guard = self.provisioning_lock.lock().await;
        if cached.exists() && !self.discard_invalid_cached_executable(&cached)? {
            return Ok(cached);
        }

        let staging_root = std::env::temp_dir()
            .join("VivoKsu")
            .join("payload-dumper")
            .join(format!("{:x}", unique_suffix()));
        fs::create_dir_all(&staging_root)
            .map_err(|error| PayloadProvisionError::Io(error.to_string()))?;

        let result = async {
            let downloader = self
                .downloader
                .as_ref()
                .ok_or(PayloadProvisionError::MissingDownloader)?;
            let zip_path = staging_root.join(PAYLOAD_DUMPER_ASSET_NAME);
            // `PAYLOAD_DUMPER_SHA256` is the digest of the *extracted*
            // `payload_dumper.exe`, not of the zip.  Pinning it onto the zip
            // spec makes every download fail its integrity check; instead
            // verify the extracted executable below (matching the WPF
            // `PayloadDumperProvisioner`, whose zip spec carries no digest).
            let spec = crate::remote_assets::RemoteAssetSpec::new(
                "payload_dumper",
                github_download_url(PAYLOAD_DUMPER_ASSET_NAME),
            );
            downloader
                .download_to_file(&spec, &zip_path, progress, cancellation_token)
                .await?;
            extract_archive_safely(&zip_path, &staging_root)?;
            let extracted = staging_root.join(PAYLOAD_DUMPER_EXECUTABLE_NAME);
            if !extracted.exists() {
                return Err(PayloadProvisionError::ExecutableMissing);
            }

            self.verify_executable_hash(&extracted)?;
            fs::create_dir_all(&self.installation_root)
                .map_err(|error| PayloadProvisionError::Io(error.to_string()))?;
            fs::copy(&extracted, &cached)
                .map_err(|error| PayloadProvisionError::Io(error.to_string()))?;
            Ok::<PathBuf, PayloadProvisionError>(cached.clone())
        }
        .await;

        let _ = fs::remove_dir_all(&staging_root);
        result
    }

    fn discard_invalid_cached_executable(
        &self,
        path: &Path,
    ) -> Result<bool, PayloadProvisionError> {
        match self.verify_executable_hash(path) {
            Ok(()) => Ok(false),
            Err(PayloadProvisionError::Integrity(_)) => {
                fs::remove_file(path)
                    .map_err(|error| PayloadProvisionError::Io(error.to_string()))?;
                Ok(true)
            }
            Err(error) => Err(error),
        }
    }

    fn verify_executable_hash(&self, path: &Path) -> Result<(), PayloadProvisionError> {
        verify_executable_hash(path, &self.expected_sha256)
    }
}

fn unique_suffix() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos())
}

fn verify_executable_hash(path: &Path, expected_sha256: &str) -> Result<(), PayloadProvisionError> {
    let actual = compute_sha256(path)?;
    if !actual.eq_ignore_ascii_case(expected_sha256) {
        return Err(PayloadProvisionError::Integrity(
            "payload_dumper.exe 完整性校验失败(SHA-256 不匹配)。".to_string(),
        ));
    }

    Ok(())
}

fn compute_sha256(path: &Path) -> Result<String, PayloadProvisionError> {
    let mut stream =
        fs::File::open(path).map_err(|error| PayloadProvisionError::Io(error.to_string()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 8192];

    loop {
        let count = stream
            .read(&mut buffer)
            .map_err(|error| PayloadProvisionError::Io(error.to_string()))?;
        if count == 0 {
            break;
        }

        hasher.update(&buffer[..count]);
    }

    Ok(format!("{:x}", hasher.finalize()))
}

fn safe_archive_relative_path(name: &str) -> Result<PathBuf, PayloadProvisionError> {
    let normalized = name.replace('\\', "/");
    let path = Path::new(&normalized);
    if normalized.is_empty()
        || path.is_absolute()
        || !path.components().all(|part| match part {
            Component::Normal(name) => name.to_str().is_some_and(is_safe_windows_file_name),
            _ => false,
        })
    {
        return Err(PayloadProvisionError::Integrity(
            "非法 zip 条目路径。".to_string(),
        ));
    }
    Ok(path.to_path_buf())
}

fn is_safe_windows_file_name(name: &str) -> bool {
    if name.trim().is_empty() || name == "." || name == ".." {
        return false;
    }
    if name.ends_with(' ') || name.ends_with('.') {
        return false;
    }
    if name.contains('/') || name.contains('\\') {
        return false;
    }
    if name
        .chars()
        .any(|ch| (ch as u32) < 32 || matches!(ch, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
    {
        return false;
    }

    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
        return false;
    }
    if stem.len() == 4
        && (stem.starts_with("COM") || stem.starts_with("LPT"))
        && stem.as_bytes()[3].is_ascii_digit()
        && (b'1'..=b'9').contains(&stem.as_bytes()[3])
    {
        return false;
    }

    true
}

fn extract_archive_safely(
    archive_path: &Path,
    destination_root: &Path,
) -> Result<(), PayloadProvisionError> {
    let file = fs::File::open(archive_path)
        .map_err(|error| PayloadProvisionError::Io(error.to_string()))?;
    let mut archive =
        ZipArchive::new(file).map_err(|error| PayloadProvisionError::Zip(error.to_string()))?;

    let mut entries = Vec::with_capacity(archive.len());
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|error| PayloadProvisionError::Zip(format!("读取 zip 条目失败: {error}")))?;
        let name = entry.name().to_string();
        let path = safe_archive_relative_path(&name)
            .map_err(|_| PayloadProvisionError::Integrity(format!("非法 zip 条目路径: {name}")))?;
        entries.push((path, entry.is_dir(), name));
    }

    fs::create_dir_all(destination_root)
        .map_err(|error| PayloadProvisionError::Io(error.to_string()))?;

    for (index, (relative_path, is_directory, name)) in entries.into_iter().enumerate() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| PayloadProvisionError::Zip(format!("读取 zip 条目失败: {error}")))?;
        let destination = destination_root.join(relative_path);
        if is_directory {
            fs::create_dir_all(&destination)
                .map_err(|error| PayloadProvisionError::Io(error.to_string()))?;
            continue;
        }

        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| PayloadProvisionError::Io(error.to_string()))?;
        }
        let mut output = fs::File::create(&destination).map_err(|error| {
            PayloadProvisionError::Io(format!("创建解压文件失败 {name}: {error}"))
        })?;
        std::io::copy(&mut entry, &mut output).map_err(|error| {
            PayloadProvisionError::Io(format!("解压 zip 条目失败 {name}: {error}"))
        })?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::{write::SimpleFileOptions, ZipWriter};

    fn temporary_payload_fixture_root() -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("nwflash-payload-fixture-{}", unique_suffix()));
        std::fs::create_dir_all(&root).expect("fixture root should be created");
        root
    }

    fn write_zip_fixture(root: &Path, entries: &[(&str, &[u8])]) -> PathBuf {
        let archive_path = root.join("payload.zip");
        let file = std::fs::File::create(&archive_path).expect("fixture archive should be created");
        let mut writer = ZipWriter::new(file);
        for (name, contents) in entries {
            writer
                .start_file(*name, SimpleFileOptions::default())
                .expect("fixture entry should be created");
            writer
                .write_all(contents)
                .expect("fixture entry should be written");
        }
        writer.finish().expect("fixture archive should be finished");
        archive_path
    }

    #[test]
    fn payload_archive_rejects_backslash_rooted_and_unc_members() {
        for name in [r"\nwflash-escape.txt", r"\\127.0.0.1\share\probe"] {
            let root = temporary_payload_fixture_root();
            let archive = write_zip_fixture(&root, &[(name, b"bad")]);
            let staging = root.join("staging");
            let error = extract_archive_safely(&archive, &staging)
                .expect_err("rooted archive member must be rejected");
            assert!(matches!(error, PayloadProvisionError::Integrity(_)));
            assert!(!staging.exists());
            std::fs::remove_dir_all(root).expect("fixture root should be removed");
        }
    }

    #[test]
    fn payload_archive_rejects_windows_namespace_members_without_creating_staging() {
        for name in ["CON", "NUL.txt", "file:stream", "trailing."] {
            let root = temporary_payload_fixture_root();
            let archive = write_zip_fixture(&root, &[(name, b"bad")]);
            let staging = root.join("staging");

            let error = extract_archive_safely(&archive, &staging)
                .expect_err("Windows-unsafe archive member must be rejected");

            assert!(matches!(error, PayloadProvisionError::Integrity(_)));
            assert!(!staging.exists());
            std::fs::remove_dir_all(root).expect("fixture root should be removed");
        }
    }

    #[test]
    fn payload_archive_extracts_relative_nested_members() {
        let root = temporary_payload_fixture_root();
        let archive = write_zip_fixture(&root, &[("bin/payload_dumper.exe", b"good")]);
        let staging = root.join("staging");

        extract_archive_safely(&archive, &staging).expect("relative member should extract");
        assert_eq!(
            std::fs::read(staging.join("bin/payload_dumper.exe"))
                .expect("extracted file should exist"),
            b"good"
        );
        std::fs::remove_dir_all(root).expect("fixture root should be removed");
    }

    #[test]
    fn corrupt_cached_payload_is_not_available() {
        let root = std::env::temp_dir().join(format!("nwflash-payload-cache-{}", unique_suffix()));
        let provisioner = PayloadDumperProvisioner::new(
            RemoteAssetDownloader::default(),
            Some(root.clone()),
            None,
        );
        std::fs::create_dir_all(&root).expect("cache root should be created");
        std::fs::write(
            provisioner.cached_executable_path(),
            b"corrupt payload cache",
        )
        .expect("corrupt cache should be written");

        assert!(!provisioner.is_available());
        std::fs::remove_dir_all(root).expect("cache root should be removed");
    }

    #[test]
    fn corrupt_cached_payload_is_removed_before_a_reinstall_attempt() {
        let root = std::env::temp_dir().join(format!("nwflash-payload-repair-{}", unique_suffix()));
        let provisioner = PayloadDumperProvisioner::new(
            RemoteAssetDownloader::default(),
            Some(root.clone()),
            None,
        );
        std::fs::create_dir_all(&root).expect("cache root should be created");
        let cached = provisioner.cached_executable_path();
        std::fs::write(&cached, b"corrupt payload cache").expect("corrupt cache should be written");

        assert!(provisioner
            .discard_invalid_cached_executable(&cached)
            .expect("corrupt cache should be removable"));
        assert!(!cached.exists());
        std::fs::remove_dir_all(root).expect("cache root should be removed");
    }

    #[test]
    fn bundled_payload_provisioner_prefers_the_explicit_resource_tree() {
        let root = temporary_payload_fixture_root();
        let provisioner = PayloadDumperProvisioner::bundled(root.clone());

        assert!(provisioner.downloader.is_none());
        assert_eq!(
            provisioner.bundled_executable_path(),
            Some(root.join("payload-tools").join(PAYLOAD_DUMPER_EXECUTABLE_NAME).as_path())
        );

        std::fs::remove_dir_all(root).expect("fixture root should be removed");
    }
}
