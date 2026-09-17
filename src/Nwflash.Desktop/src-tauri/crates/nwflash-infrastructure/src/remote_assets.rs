//! Remote resource catalog constants for out-of-band assets.

//! Mirrors are ordered exactly as the original WPF client: direct GitHub host first, then
//! the fallback HTTP mirrors.

/// GitHub 仓库拥有者与发布名称（发布瘦身后，scrcpy/AKI/payload_dumper 改为按需拉取）。
pub const OWNER: &str = "xxz13352";
pub const REPOSITORY_NAME: &str = "NWFlash";
pub const RELEASE_TAG: &str = "v1.0.0";

/// 镜像列表：直连 github 失败时按顺序退避。
pub const MIRRORS: [&str; 3] = [
    "https://gh-proxy.com/",
    "https://ghfast.top/",
    "https://ghproxy.net/",
];

/// payload_dumper 发布资产名与入口 exe 名称。
pub const PAYLOAD_DUMPER_ASSET_NAME: &str = "payload_dumper-win-x64.zip";
pub const PAYLOAD_DUMPER_EXECUTABLE_NAME: &str = "payload_dumper.exe";

/// payload_dumper 期望 SHA-256（与发布资源绑定，单位：小写十六进制）。
pub const PAYLOAD_DUMPER_SHA256: &str =
    "031b404609e804cd620fb10efdfce577b633f8b0ad8029fbd7170be3bc4cbe82";

/// ROOT 管理器 APK 文件名（与随包一致，避免命名漂移）。
pub const ROOT_MANAGER_APK_KSU: &str = "KSU.APK";
pub const ROOT_MANAGER_APK_OFFICIAL: &str = "KernelSU.apk";

/// ROOT 管理器对应的 SHA-256。
pub const ROOT_MANAGER_SHA256_KSU: &str =
    "43ebb3e3cbc885285bd824f351e5cca2169a4435c8bd0268584ad3c9d7248d4a";
pub const ROOT_MANAGER_SHA256_OFFICIAL: &str =
    "dca1cf72a6f6cff4a116242fbe940a161099bafbd9d74ca4518756eaad5c8c03";

/// 运行时 key 常量。
pub const MANAGER_KEY_KSU: &str = "KSU";
pub const MANAGER_KEY_OFFICIAL: &str = "OfficialKsu";

/// 仅用于 Android 内核匹配。
pub const SUPPORTED_KERNEL_RELEASE_FAMILIES: [&str; 3] =
    ["android13-5.15", "android14-6.1", "android15-6.6"];

/// 远端资产参数模型（名称 + 下载源 + 可选校验信息）。
#[derive(Debug, Clone)]
pub struct RemoteAssetSpec {
    pub display_name: String,
    pub github_url: String,
    pub expected_sha256: Option<String>,
    pub expected_length: Option<u64>,
}

impl RemoteAssetSpec {
    pub fn new(display_name: impl Into<String>, github_url: impl Into<String>) -> Self {
        Self {
            display_name: display_name.into(),
            github_url: github_url.into(),
            expected_sha256: None,
            expected_length: None,
        }
    }

    pub fn with_expected_sha256(mut self, expected_sha256: impl Into<String>) -> Self {
        self.expected_sha256 = Some(expected_sha256.into());
        self
    }

    pub fn with_expected_length(mut self, expected_length: u64) -> Self {
        self.expected_length = Some(expected_length);
        self
    }
}

/// 组装 GitHub Release 资产下载链接。
pub fn github_download_url(asset_name: &str) -> String {
    format!(
        "https://github.com/{}/{}/releases/download/{}/{}",
        OWNER, REPOSITORY_NAME, RELEASE_TAG, asset_name
    )
}

pub fn manager_apk_filename(key: &str) -> Option<&'static str> {
    match key {
        MANAGER_KEY_KSU => Some(ROOT_MANAGER_APK_KSU),
        MANAGER_KEY_OFFICIAL => Some(ROOT_MANAGER_APK_OFFICIAL),
        _ => None,
    }
}

pub fn manager_apk_sha256(key: &str) -> Option<&'static str> {
    match key {
        MANAGER_KEY_KSU => Some(ROOT_MANAGER_SHA256_KSU),
        MANAGER_KEY_OFFICIAL => Some(ROOT_MANAGER_SHA256_OFFICIAL),
        _ => None,
    }
}

/// 验证 manager key 是否为已知项。
pub fn is_known_manager_key(key: &str) -> bool {
    matches!(key, MANAGER_KEY_KSU | MANAGER_KEY_OFFICIAL)
}
