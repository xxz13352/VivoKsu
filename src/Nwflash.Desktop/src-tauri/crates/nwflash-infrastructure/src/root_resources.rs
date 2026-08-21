use std::{
    collections::HashMap,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use zip::ZipArchive;

use crate::{
    paths::resource_root,
    remote_assets::{
        github_download_url, is_known_manager_key, manager_apk_filename, manager_apk_sha256,
        RemoteAssetSpec, SUPPORTED_KERNEL_RELEASE_FAMILIES,
    },
    resource_downloader::{ProgressSink, RemoteAssetDownloader, ResourceDownloadError},
};

#[derive(Debug, Clone, Serialize)]
pub struct VivoRootLibrarySpec;

#[derive(Debug, Clone, Serialize)]
pub struct VivoRootToolResource {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct VivoRootManagerResource {
    pub key: String,
    pub apk_path: String,
    pub package_name: String,
    pub activity_name: String,
    pub libraries: HashMap<String, VivoRootLibrarySpec>,
}

#[derive(Debug, Error)]
pub enum RootResourceError {
    #[error("不支持的 ROOT 管理器: {0}")]
    UnknownManager(String),
    #[error("文件缺失: {0}")]
    MissingFile(String),
    #[error("文件为空: {0}")]
    EmptyFile(String),
    #[error("IO: {0}")]
    Io(String),
    #[error("下载失败: {0}")]
    Download(#[from] ResourceDownloadError),
    #[error("完整性校验失败: {0}")]
    Integrity(String),
    #[error("ZIP 解析失败: {0}")]
    Zip(String),
    #[error("无效的 APK 结构: {0}")]
    InvalidApk(String),
    #[error("参数错误: {0}")]
    InvalidArgument(String),
    #[error("无效 KMI: {0}")]
    InvalidKmi(String),
    #[error("库文件缺失: {0}")]
    MissingLibrary(String),
}

pub struct VivoRootResourceService {
    project_root: PathBuf,
    downloader: Option<RemoteAssetDownloader>,
}

impl VivoRootResourceService {
    const DEFAULT_CACHE_APK_NAME: &'static str = "apk";
    const KSU_PACKAGE_NAME: &'static str = "me.inkdye.vivoksu";
    const KSU_ACTIVITY_NAME: &'static str = "me.inkdye.vivoksu.ui.MainActivity";
    const OFFICIAL_PACKAGE_NAME: &'static str = "me.weishu.kernelsu";
    const OFFICIAL_ACTIVITY_NAME: &'static str = "me.weishu.kernelsu.ui.MainActivity";

    pub fn new(project_root: PathBuf, downloader: Option<RemoteAssetDownloader>) -> Self {
        Self {
            project_root,
            downloader,
        }
    }

    pub fn supported_kmis() -> &'static [&'static str] {
        &SUPPORTED_KERNEL_RELEASE_FAMILIES
    }

    pub fn manager_keys() -> Vec<&'static str> {
        vec!["KSU", "OfficialKsu"]
    }

    pub fn resolve_manager(&self, key: &str) -> Result<VivoRootManagerResource, RootResourceError> {
        if !is_known_manager_key(key) {
            return Err(RootResourceError::UnknownManager(key.to_string()));
        }

        let bundled = self.project_root.join("apk").join(apk_file_name(key)?);
        let cached = self.cache_apk_path(key);
        let apk_path = select_verified_manager_apk(&bundled, &cached, |path| {
            verify_manager_apk_file(path, key, manager_apk_sha256(key))
        })
        .unwrap_or(cached)
        .to_string_lossy()
        .to_string();

        let mut libraries = HashMap::new();
        libraries.insert("arm64-v8a".to_string(), VivoRootLibrarySpec);
        libraries.insert("x86_64".to_string(), VivoRootLibrarySpec);

        Ok(VivoRootManagerResource {
            key: key.to_string(),
            apk_path,
            package_name: if key == "KSU" {
                Self::KSU_PACKAGE_NAME.to_string()
            } else {
                Self::OFFICIAL_PACKAGE_NAME.to_string()
            },
            activity_name: if key == "KSU" {
                Self::KSU_ACTIVITY_NAME.to_string()
            } else {
                Self::OFFICIAL_ACTIVITY_NAME.to_string()
            },
            libraries,
        })
    }

    pub fn is_manager_apk_installed(&self, key: &str) -> bool {
        if !is_known_manager_key(key) {
            return false;
        }

        self.resolve_manager(key)
            .is_ok_and(|manager| self.verify_manager_apk(&manager).is_ok())
    }

    pub async fn ensure_manager_apk(
        &self,
        manager: &VivoRootManagerResource,
        cancellation_token: &CancellationToken,
        progress: Option<&ProgressSink>,
    ) -> Result<VivoRootManagerResource, RootResourceError> {
        match self.verify_manager_apk(manager) {
            Ok(()) => return Ok(manager.clone()),
            Err(
                RootResourceError::MissingFile(_)
                | RootResourceError::EmptyFile(_)
                | RootResourceError::Integrity(_)
                | RootResourceError::Zip(_)
                | RootResourceError::InvalidApk(_),
            ) => {}
            Err(error) => return Err(error),
        }

        let downloader = self.downloader.as_ref().ok_or_else(|| {
            RootResourceError::InvalidArgument(format!(
                "{} 管理器 APK 缺失且未配置下载器。",
                manager.key
            ))
        })?;

        let cache_path = self.cache_apk_path(&manager.key);
        let mut spec = RemoteAssetSpec::new(
            format!("{} 管理器 APK", manager.key),
            github_download_url(apk_file_name(&manager.key)?),
        );
        if let Some(expected_hash) = manager_apk_sha256(&manager.key) {
            spec = spec.with_expected_sha256(expected_hash);
        }

        downloader
            .download_to_file(&spec, &cache_path, progress, cancellation_token)
            .await?;

        let updated = self.resolve_manager(&manager.key)?;
        self.verify_manager_apk(&updated)?;
        Ok(updated)
    }

    pub fn resolve_magiskboot(&self) -> VivoRootToolResource {
        VivoRootToolResource {
            name: "magiskboot".to_string(),
            path: self
                .project_root
                .join("root-tools")
                .join("magiskboot.so")
                .to_string_lossy()
                .to_string(),
        }
    }

    pub fn verify_root_tool(&self, tool: &VivoRootToolResource) -> Result<(), RootResourceError> {
        let path = Path::new(&tool.path);
        if !path.exists() {
            return Err(RootResourceError::MissingFile(format!(
                "未找到 {} 工具。",
                tool.name
            )));
        }

        if fs::metadata(path)
            .map(|meta| meta.len())
            .unwrap_or_default()
            == 0
        {
            return Err(RootResourceError::EmptyFile(format!(
                "{} 工具为空。",
                tool.name
            )));
        }

        Ok(())
    }

    pub fn validate_kmi(kmi: &str) -> Result<&str, RootResourceError> {
        if SUPPORTED_KERNEL_RELEASE_FAMILIES.contains(&kmi) {
            return Ok(kmi);
        }

        Err(RootResourceError::InvalidKmi(format!(
            "不支持的 KMI: {kmi}"
        )))
    }

    pub fn map_kernel_release(release: &str) -> Result<&'static str, RootResourceError> {
        let normalized = release.trim();
        if normalized.starts_with("5.15.") {
            return Ok("android13-5.15");
        }
        if normalized.starts_with("6.1.") {
            return Ok("android14-6.1");
        }
        if normalized.starts_with("6.6.") {
            return Ok("android15-6.6");
        }

        Err(RootResourceError::InvalidKmi(format!(
            "无法映射 Vivo KernelSU KMI: {release}",
        )))
    }

    pub fn verify_manager_apk(
        &self,
        manager: &VivoRootManagerResource,
    ) -> Result<(), RootResourceError> {
        verify_manager_apk_file(
            Path::new(&manager.apk_path),
            &manager.key,
            manager_apk_sha256(&manager.key),
        )
    }

    pub fn extract_verified_libksud(
        &self,
        manager: &VivoRootManagerResource,
        abi: &str,
        destination: &Path,
    ) -> Result<PathBuf, RootResourceError> {
        if !manager.libraries.contains_key(abi) {
            return Err(RootResourceError::MissingLibrary(format!(
                "{} 不支持设备 ABI: {abi}",
                manager.key
            )));
        }

        self.verify_manager_apk(manager)?;
        let file = fs::File::open(&manager.apk_path)
            .map_err(|error| RootResourceError::Io(error.to_string()))?;
        let mut archive = ZipArchive::new(file)
            .map_err(|error| RootResourceError::Zip(format!("读取 APK 失败: {error}")))?;

        let entry_name = format!("lib/{abi}/libksud.so");
        let index = (0..archive.len())
            .find_map(|i| {
                let entry = archive.by_index(i).ok()?;
                if entry.name() == entry_name {
                    Some(i)
                } else {
                    None
                }
            })
            .ok_or_else(|| {
                RootResourceError::MissingFile(format!("APK 中必须存在唯一的 {entry_name}。"))
            })?;

        let mut entry = archive
            .by_index(index)
            .map_err(|error| RootResourceError::Zip(format!("读取 {entry_name} 失败: {error}")))?;

        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(|error| RootResourceError::Io(error.to_string()))?;
        }

        let pending = destination.with_extension("pending");
        {
            let mut output = fs::File::create(&pending)
                .map_err(|error| RootResourceError::Io(error.to_string()))?;
            std::io::copy(&mut entry, &mut output)
                .map_err(|error| RootResourceError::Io(error.to_string()))?;
            output
                .flush()
                .map_err(|error| RootResourceError::Io(error.to_string()))?;
        }

        if fs::metadata(&pending)
            .map(|meta| meta.len())
            .unwrap_or_default()
            == 0
        {
            let _ = fs::remove_file(&pending);
            return Err(RootResourceError::EmptyFile(
                "APK 中的 libksud.so 为空。".to_string(),
            ));
        }

        fs::rename(&pending, destination)
            .map_err(|error| RootResourceError::Io(error.to_string()))?;
        Ok(destination.to_path_buf())
    }

    pub fn cache_apk_directory() -> PathBuf {
        resource_root().join(Self::DEFAULT_CACHE_APK_NAME)
    }

    fn cache_apk_path(&self, key: &str) -> PathBuf {
        Self::cache_apk_directory().join(apk_file_name(key).unwrap_or_default())
    }
}

fn apk_file_name(key: &str) -> Result<&'static str, RootResourceError> {
    manager_apk_filename(key).ok_or_else(|| RootResourceError::UnknownManager(key.to_string()))
}

fn compute_sha256(path: &Path) -> Result<String, RootResourceError> {
    let mut stream =
        fs::File::open(path).map_err(|error| RootResourceError::Io(error.to_string()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 8192];

    loop {
        let count = stream
            .read(&mut buffer)
            .map_err(|error| RootResourceError::Io(error.to_string()))?;
        if count == 0 {
            break;
        }

        hasher.update(&buffer[..count]);
    }

    Ok(format!("{:x}", hasher.finalize()))
}

fn select_verified_manager_apk<F>(bundled: &Path, cached: &Path, mut verify: F) -> Option<PathBuf>
where
    F: FnMut(&Path) -> Result<(), RootResourceError>,
{
    for candidate in [bundled, cached] {
        if verify(candidate).is_ok() {
            return Some(candidate.to_path_buf());
        }
    }

    None
}

fn verify_manager_apk_file(
    path: &Path,
    manager_key: &str,
    expected_hash: Option<&str>,
) -> Result<(), RootResourceError> {
    if !path.exists() {
        return Err(RootResourceError::MissingFile(format!(
            "未找到 {manager_key} 管理器 APK。"
        )));
    }

    if fs::metadata(path)
        .map(|meta| meta.len())
        .unwrap_or_default()
        == 0
    {
        return Err(RootResourceError::EmptyFile(format!(
            "{manager_key} 管理器 APK 为空。"
        )));
    }

    if let Some(expected_hash) = expected_hash {
        let actual_hash = compute_sha256(path)?;
        if !actual_hash.eq_ignore_ascii_case(expected_hash) {
            return Err(RootResourceError::Integrity(format!(
                "{manager_key} 管理器 APK 完整性校验失败（SHA-256 不匹配）。"
            )));
        }
    }

    let mut archive = ZipArchive::new(fs::File::open(path).map_err(|error| {
        RootResourceError::Io(format!("打开 {manager_key} 管理器 APK 失败: {error}"))
    })?)
    .map_err(|error| RootResourceError::Zip(error.to_string()))?;

    let mut has_manifest = false;
    for index in 0..archive.len() {
        let entry = archive.by_index(index).map_err(|error| {
            RootResourceError::Zip(format!("{manager_key} 管理器 APK 不可读: {error}"))
        })?;
        if entry.name() == "AndroidManifest.xml" {
            has_manifest = true;
            break;
        }
    }
    if !has_manifest {
        return Err(RootResourceError::InvalidApk(format!(
            "{manager_key} 管理器 APK 不是有效的 APK（缺少 AndroidManifest.xml）。"
        )));
    }

    Ok(())
}

impl Clone for VivoRootResourceService {
    fn clone(&self) -> Self {
        Self {
            project_root: self.project_root.clone(),
            downloader: self.downloader.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs::{self, File},
        io::Write,
        time::{SystemTime, UNIX_EPOCH},
    };

    use zip::{write::SimpleFileOptions, ZipWriter};

    use super::*;

    fn test_root(label: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be available")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("nwflash-{label}-{suffix}"));
        fs::create_dir_all(&root).expect("test root should be created");
        root
    }

    fn write_valid_apk(path: &Path) {
        let parent = path.parent().expect("APK fixture should have a parent");
        fs::create_dir_all(parent).expect("APK fixture parent should exist");
        let mut archive = ZipWriter::new(File::create(path).expect("APK fixture should open"));
        archive
            .start_file("AndroidManifest.xml", SimpleFileOptions::default())
            .expect("manifest entry should be created");
        archive
            .write_all(b"manifest")
            .expect("manifest fixture should be written");
        archive.finish().expect("APK fixture should be finalized");
    }

    #[test]
    fn manager_selection_prefers_a_verified_cache_over_an_invalid_bundle() {
        let root = test_root("root-manager-selection");
        let bundled = root.join("apk").join("manager.apk");
        let cached = root.join("cache").join("manager.apk");
        fs::create_dir_all(bundled.parent().expect("bundle should have a parent"))
            .expect("bundle parent should exist");
        fs::write(&bundled, b"invalid bundle").expect("invalid bundle should be written");
        write_valid_apk(&cached);
        let expected_hash = compute_sha256(&cached).expect("cache hash should be available");

        let selected = select_verified_manager_apk(&bundled, &cached, |path| {
            verify_manager_apk_file(path, "fixture", Some(&expected_hash))
        })
        .expect("verified cache should be selected");

        assert_eq!(selected, cached);
        fs::remove_dir_all(root).expect("test root should be removed");
    }
}
