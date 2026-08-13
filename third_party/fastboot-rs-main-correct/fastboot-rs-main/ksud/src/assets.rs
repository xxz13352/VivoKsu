use anyhow::{Context, Result};
use rust_embed::RustEmbed;
use serde_json::Value;
use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::io::Read;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::Duration;

const GITHUB_API: &str = "https://api.github.com";
const GITHUB_ORIGIN: &str = "https://github.com";
const MAX_RELEASE_JSON_SIZE: usize = 2 * 1024 * 1024;
const MAX_KERNEL_MODULE_SIZE: usize = 8 * 1024 * 1024;
const FREE_DOWNLOAD_PREFIXES: [&str; 2] = ["https://gh-proxy.com", "https://ghfast.top"];

#[cfg(target_os = "android")]
mod android {
    use crate::assets::Asset;
    use crate::defs::BINARY_DIR;
    use crate::utils::ensure_binary;
    use const_format::concatcp;

    pub const RESETPROP_PATH: &str = concatcp!(BINARY_DIR, "resetprop");
    pub const BUSYBOX_PATH: &str = concatcp!(BINARY_DIR, "busybox");
    pub const BOOTCTL_PATH: &str = concatcp!(BINARY_DIR, "bootctl");

    pub fn ensure_binaries(ignore_if_exist: bool) -> anyhow::Result<()> {
        for file in Asset::iter() {
            if file == "ksuinit" || file.ends_with(".ko") {
                // don't extract ksuinit and kernel modules
                continue;
            }
            let asset =
                Asset::get(&file).ok_or_else(|| anyhow::anyhow!("asset not found: {file}"))?;
            ensure_binary(format!("{BINARY_DIR}{file}"), &asset.data, ignore_if_exist)?;
        }
        Ok(())
    }
}

#[cfg(target_os = "android")]
pub use android::*;

#[cfg(all(target_arch = "x86_64", target_os = "android"))]
#[derive(RustEmbed)]
#[folder = "bin/x86_64"]
struct Asset;

// IF NOT x86_64 ANDROID, ie. macos, linux, windows, always use aarch64
#[cfg(not(all(target_arch = "x86_64", target_os = "android")))]
#[derive(RustEmbed)]
#[folder = "bin/aarch64"]
struct Asset;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KernelSuDistribution {
    Official,
    SukiSuUltra,
    KernelSuNext,
    WildKsu,
}

impl KernelSuDistribution {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Official => "kernelsu",
            Self::SukiSuUltra => "sukisu-ultra",
            Self::KernelSuNext => "kernelsu-next",
            Self::WildKsu => "wild-ksu",
        }
    }

    const fn repository(self) -> &'static str {
        match self {
            Self::Official => "tiann/KernelSU",
            Self::SukiSuUltra => "SukiSU-Ultra/SukiSU-Ultra",
            Self::KernelSuNext => "KernelSU-Next/KernelSU-Next",
            Self::WildKsu => "WildKernels/Wild_KSU",
        }
    }
}

pub fn get_asset_data(name: &str) -> Result<std::borrow::Cow<'static, [u8]>> {
    let asset = Asset::get(name).ok_or_else(|| anyhow::anyhow!("asset not found: {name}"))?;
    Ok(asset.data)
}

pub fn copy_assets_to_file(name: &str, dst: impl AsRef<Path>) -> Result<()> {
    let data = get_asset_data(name)?;
    std::fs::write(dst, &*data)?;
    Ok(())
}

#[derive(Debug, Eq, PartialEq)]
struct RemoteAsset {
    tag: String,
    name: String,
    api_url: Option<String>,
    url: String,
    sha256: String,
    size: Option<usize>,
}

fn validate_release_component(value: &str, label: &str, max_len: usize) -> Result<()> {
    anyhow::ensure!(
        !value.is_empty()
            && value.len() <= max_len
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')),
        "非法 {label}: {value:?}"
    );
    Ok(())
}

fn parse_release_asset(
    distribution: KernelSuDistribution,
    requested_name: &str,
    json: &[u8],
    origin: &str,
) -> Result<RemoteAsset> {
    validate_release_component(requested_name, "release 文件名", 128)?;
    anyhow::ensure!(
        json.len() <= MAX_RELEASE_JSON_SIZE,
        "GitHub release 元数据超过 {MAX_RELEASE_JSON_SIZE} 字节"
    );

    let root: Value = serde_json::from_slice(json).context("解析 GitHub release 元数据失败")?;
    let tag = root
        .get("tag_name")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("GitHub release 缺少 tag_name"))?;
    validate_release_component(tag, "release tag", 64)?;

    let assets = root
        .get("assets")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("GitHub release 缺少 assets"))?;
    anyhow::ensure!(assets.len() <= 512, "GitHub release 资源数量异常");
    let asset = assets
        .iter()
        .find(|asset| asset.get("name").and_then(Value::as_str) == Some(requested_name))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{} 最新版本 {tag} 不包含 {requested_name}",
                distribution.name()
            )
        })?;

    let digest = asset
        .get("digest")
        .and_then(Value::as_str)
        .and_then(|value| value.strip_prefix("sha256:"))
        .ok_or_else(|| anyhow::anyhow!("{requested_name} 缺少 SHA-256"))?;
    anyhow::ensure!(
        digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "{requested_name} 的 SHA-256 非法"
    );

    let size = asset
        .get("size")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| anyhow::anyhow!("{requested_name} 缺少有效文件大小"))?;
    anyhow::ensure!(
        size > 0 && size <= MAX_KERNEL_MODULE_SIZE,
        "{requested_name} 文件大小异常: {size}"
    );

    let url = asset
        .get("browser_download_url")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("{requested_name} 缺少下载地址"))?;
    let expected_url = format!(
        "{origin}/{}/releases/download/{tag}/{requested_name}",
        distribution.repository()
    );
    anyhow::ensure!(url == expected_url, "{requested_name} 下载地址不可信");
    let api_url = asset
        .get("url")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("{requested_name} 缺少 API 下载地址"))?;
    let api_prefix = format!(
        "{GITHUB_API}/repos/{}/releases/assets/",
        distribution.repository()
    );
    let asset_id = api_url
        .strip_prefix(&api_prefix)
        .ok_or_else(|| anyhow::anyhow!("{requested_name} API 下载地址不可信"))?;
    anyhow::ensure!(
        !asset_id.is_empty()
            && asset_id.len() <= 20
            && asset_id.bytes().all(|byte| byte.is_ascii_digit()),
        "{requested_name} API asset id 非法"
    );

    Ok(RemoteAsset {
        tag: tag.to_string(),
        name: requested_name.to_string(),
        api_url: Some(api_url.to_string()),
        url: url.to_string(),
        sha256: digest.to_ascii_lowercase(),
        size: Some(size),
    })
}

fn parse_release_asset_html(
    distribution: KernelSuDistribution,
    requested_name: &str,
    tag: &str,
    html: &[u8],
    origin: &str,
) -> Result<RemoteAsset> {
    validate_release_component(requested_name, "release 文件名", 128)?;
    validate_release_component(tag, "release tag", 64)?;
    anyhow::ensure!(
        html.len() <= MAX_RELEASE_JSON_SIZE,
        "GitHub release 页面超过 {MAX_RELEASE_JSON_SIZE} 字节"
    );
    let html = std::str::from_utf8(html).context("GitHub release 页面非 UTF-8")?;
    let path = format!(
        "/{}/releases/download/{tag}/{requested_name}",
        distribution.repository()
    );
    let pattern = format!(
        r#"(?s)<li[^>]*>.*?<a href="{}"[^>]*>.*?sha256:([0-9a-fA-F]{{64}}).*?</li>"#,
        regex_lite::escape(&path)
    );
    let captures = regex_lite::Regex::new(&pattern)
        .context("构造 release 资源匹配规则失败")?
        .captures(html)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{} 最新版本 {tag} 不包含 {requested_name}",
                distribution.name()
            )
        })?;
    let digest = captures
        .get(1)
        .ok_or_else(|| anyhow::anyhow!("{requested_name} 缺少 SHA-256"))?
        .as_str()
        .to_ascii_lowercase();

    Ok(RemoteAsset {
        tag: tag.to_string(),
        name: requested_name.to_string(),
        api_url: None,
        url: format!("{origin}{path}"),
        sha256: digest,
        size: None,
    })
}

fn parse_release_kmis(json: &[u8]) -> Result<Vec<String>> {
    anyhow::ensure!(
        json.len() <= MAX_RELEASE_JSON_SIZE,
        "GitHub release 元数据超过 {MAX_RELEASE_JSON_SIZE} 字节"
    );
    let root: Value = serde_json::from_slice(json).context("解析 GitHub release 元数据失败")?;
    let assets = root
        .get("assets")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("GitHub release 缺少 assets"))?;
    anyhow::ensure!(assets.len() <= 512, "GitHub release 资源数量异常");

    let mut kmis = BTreeSet::new();
    for asset in assets {
        let Some(name) = asset.get("name").and_then(Value::as_str) else {
            continue;
        };
        let Some(kmi) = name.strip_suffix("_kernelsu.ko") else {
            continue;
        };
        validate_release_component(kmi, "KMI", 32)?;
        kmis.insert(kmi.to_string());
    }
    anyhow::ensure!(!kmis.is_empty(), "最新 Release 不包含 KernelSU KO");
    Ok(kmis.into_iter().collect())
}

fn parse_release_kmis_html(
    distribution: KernelSuDistribution,
    tag: &str,
    html: &[u8],
) -> Result<Vec<String>> {
    validate_release_component(tag, "release tag", 64)?;
    anyhow::ensure!(
        html.len() <= MAX_RELEASE_JSON_SIZE,
        "GitHub release 页面超过 {MAX_RELEASE_JSON_SIZE} 字节"
    );
    let html = std::str::from_utf8(html).context("GitHub release 页面非 UTF-8")?;
    let pattern = format!(
        r#"/{}/releases/download/{}/([A-Za-z0-9._-]+)_kernelsu\.ko"#,
        regex_lite::escape(distribution.repository()),
        regex_lite::escape(tag)
    );
    let regex = regex_lite::Regex::new(&pattern).context("构造 KMI 匹配规则失败")?;
    let mut kmis = BTreeSet::new();
    for captures in regex.captures_iter(html) {
        let kmi = captures
            .get(1)
            .ok_or_else(|| anyhow::anyhow!("Release KMI 资源格式错误"))?
            .as_str();
        validate_release_component(kmi, "KMI", 32)?;
        kmis.insert(kmi.to_string());
    }
    anyhow::ensure!(!kmis.is_empty(), "最新 Release 不包含 KernelSU KO");
    Ok(kmis.into_iter().collect())
}

#[cfg(windows)]
fn windows_system_proxy() -> Option<String> {
    const KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings";

    let enabled = Command::new("reg")
        .args(["query", KEY, "/v", "ProxyEnable"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .is_some_and(|output| output.split_whitespace().any(|field| field == "0x1"));
    if !enabled {
        return None;
    }

    let output = Command::new("reg")
        .args(["query", KEY, "/v", "ProxyServer"])
        .output()
        .ok()
        .filter(|output| output.status.success())?;
    let output = String::from_utf8(output.stdout).ok()?;
    let value = output
        .lines()
        .find(|line| line.contains("ProxyServer"))?
        .split("REG_SZ")
        .nth(1)?
        .trim();
    let proxy = value
        .split(';')
        .find_map(|entry| entry.trim().strip_prefix("https="))
        .or_else(|| {
            value
                .split(';')
                .find_map(|entry| entry.trim().strip_prefix("http="))
        })
        .unwrap_or(value)
        .trim();
    (!proxy.is_empty()).then(|| proxy.to_string())
}

#[cfg(not(windows))]
fn windows_system_proxy() -> Option<String> {
    None
}

fn build_http_agent() -> Result<ureq::Agent> {
    let mut builder = ureq::AgentBuilder::new()
        .user_agent("ksud-runtime-assets/1")
        .timeout_connect(Duration::from_secs(20))
        .timeout_read(Duration::from_mins(2))
        .timeout_write(Duration::from_mins(2));
    let disable_proxy = proxy_disabled();
    let proxy = (!disable_proxy)
        .then(|| {
            env::var("KSU_HTTP_PROXY")
                .ok()
                .or_else(|| env::var("HTTPS_PROXY").ok())
                .or_else(|| env::var("https_proxy").ok())
                .or_else(|| env::var("ALL_PROXY").ok())
                .or_else(|| env::var("all_proxy").ok())
                .or_else(windows_system_proxy)
        })
        .flatten();

    if let Some(proxy) = proxy.filter(|value| !value.trim().is_empty()) {
        let proxy = if proxy.contains("://") {
            proxy
        } else {
            format!("http://{proxy}")
        };
        builder = builder.proxy(ureq::Proxy::new(&proxy).context("代理地址无效")?);
    }
    Ok(builder.build())
}

fn proxy_disabled() -> bool {
    env::var("KSU_NO_PROXY").is_ok_and(|value| {
        value == "1" || value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("yes")
    })
}

fn retry_delay(response: &ureq::Response, attempt: u64) -> Duration {
    let seconds = response
        .header("Retry-After")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(attempt * 2)
        .min(60);
    Duration::from_secs(seconds)
}

fn github_token() -> Result<Option<String>> {
    Ok(env::var("GITHUB_TOKEN")
        .ok()
        .or_else(|| env::var("GH_TOKEN").ok())
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 1024
                && !value.bytes().any(|byte| matches!(byte, b'\r' | b'\n'))
        }))
}

fn request_bytes(
    agent: &ureq::Agent,
    url: &str,
    max_size: usize,
    authenticated: bool,
    accept: &str,
) -> Result<(String, Vec<u8>)> {
    let token = if authenticated { github_token()? } else { None };
    let mut last_error = None;

    for attempt in 1..=3 {
        let mut request = agent
            .get(url)
            .set("Accept", accept)
            .set("X-GitHub-Api-Version", "2022-11-28");
        if let Some(token) = &token {
            request = request.set("Authorization", &format!("Bearer {token}"));
        }

        let response = match request.call() {
            Ok(response) => response,
            Err(ureq::Error::Status(code, response)) if code == 403 || code == 429 => {
                last_error = Some(anyhow::anyhow!("GitHub 限流: HTTP {code}"));
                if authenticated {
                    break;
                }
                if attempt < 3 {
                    thread::sleep(retry_delay(&response, attempt));
                    continue;
                }
                break;
            }
            Err(error) => {
                last_error = Some(anyhow::anyhow!("请求 {url} 失败: {error}"));
                if attempt < 3 {
                    thread::sleep(Duration::from_millis(attempt * 500));
                    continue;
                }
                break;
            }
        };

        if let Some(length) = response
            .header("Content-Length")
            .and_then(|value| value.parse::<usize>().ok())
        {
            anyhow::ensure!(length <= max_size, "响应体超过 {max_size} 字节");
        }
        let final_url = response.get_url().to_string();
        let mut bytes = Vec::with_capacity(max_size.min(512 * 1024));
        response
            .into_reader()
            .take((max_size + 1) as u64)
            .read_to_end(&mut bytes)
            .with_context(|| format!("读取 {url} 失败"))?;
        anyhow::ensure!(bytes.len() <= max_size, "响应体超过 {max_size} 字节");
        return Ok((final_url, bytes));
    }

    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("请求 {url} 失败")))
}

fn latest_release_asset_from_html(
    agent: &ureq::Agent,
    distribution: KernelSuDistribution,
    requested_name: &str,
    origin: &str,
) -> Result<RemoteAsset> {
    let latest_url = format!("{origin}/{}/releases/latest", distribution.repository());
    let (final_url, _) = request_bytes(
        agent,
        &latest_url,
        MAX_RELEASE_JSON_SIZE,
        false,
        "text/html",
    )?;
    let tag = final_url
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .ok_or_else(|| anyhow::anyhow!("无法从 {final_url} 提取 release tag"))?;
    validate_release_component(tag, "release tag", 64)?;

    let assets_url = format!(
        "{origin}/{}/releases/expanded_assets/{tag}",
        distribution.repository()
    );
    let (_, html) = request_bytes(
        agent,
        &assets_url,
        MAX_RELEASE_JSON_SIZE,
        false,
        "text/html",
    )?;
    parse_release_asset_html(distribution, requested_name, tag, &html, origin)
}

fn release_kmis_from_html(
    agent: &ureq::Agent,
    distribution: KernelSuDistribution,
    origin: &str,
) -> Result<Vec<String>> {
    let latest_url = format!("{origin}/{}/releases/latest", distribution.repository());
    let (final_url, _) = request_bytes(
        agent,
        &latest_url,
        MAX_RELEASE_JSON_SIZE,
        false,
        "text/html",
    )?;
    let tag = final_url
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .ok_or_else(|| anyhow::anyhow!("无法从 {final_url} 提取 release tag"))?;
    validate_release_component(tag, "release tag", 64)?;
    let assets_url = format!(
        "{origin}/{}/releases/expanded_assets/{tag}",
        distribution.repository()
    );
    let (_, html) = request_bytes(
        agent,
        &assets_url,
        MAX_RELEASE_JSON_SIZE,
        false,
        "text/html",
    )?;
    parse_release_kmis_html(distribution, tag, &html)
}

pub fn fetch_distribution_kmis(distribution: KernelSuDistribution) -> Result<Vec<String>> {
    let origin = env::var("KSU_GITHUB_BASE_URL").unwrap_or_else(|_| GITHUB_ORIGIN.to_string());
    anyhow::ensure!(origin.starts_with("https://"), "GitHub 地址必须使用 HTTPS");
    let agent = build_http_agent()?;
    let metadata_url = format!(
        "{GITHUB_API}/repos/{}/releases/latest",
        distribution.repository()
    );
    request_bytes(
        &agent,
        &metadata_url,
        MAX_RELEASE_JSON_SIZE,
        true,
        "application/vnd.github+json",
    )
    .and_then(|(_, json)| parse_release_kmis(&json))
    .or_else(|api_error| {
        log::warn!("GitHub API KMI 查询失败，回退到公开 release 页面: {api_error}");
        release_kmis_from_html(&agent, distribution, origin.trim_end_matches('/'))
    })
}

fn prefixed_url(prefix: &str, url: &str) -> String {
    format!("{}/{}", prefix.trim_end_matches('/'), url)
}

fn download_candidates(asset: &RemoteAsset) -> Vec<(String, bool)> {
    let mut candidates = Vec::with_capacity(5);
    if let Ok(prefix) = env::var("KSU_DOWNLOAD_PREFIX")
        && !prefix.trim().is_empty()
    {
        candidates.push((prefixed_url(prefix.trim(), &asset.url), false));
    }
    if let Some(api_url) = &asset.api_url {
        candidates.push((api_url.clone(), true));
    }
    if proxy_disabled() {
        candidates.extend(
            FREE_DOWNLOAD_PREFIXES
                .iter()
                .map(|prefix| (prefixed_url(prefix, &asset.url), false)),
        );
        candidates.push((asset.url.clone(), false));
    } else {
        candidates.push((asset.url.clone(), false));
        candidates.extend(
            FREE_DOWNLOAD_PREFIXES
                .iter()
                .map(|prefix| (prefixed_url(prefix, &asset.url), false)),
        );
    }
    candidates.dedup_by(|left, right| left.0 == right.0);
    candidates
}

fn validate_download(asset: &RemoteAsset, bytes: Vec<u8>) -> Result<Vec<u8>> {
    if let Some(expected_size) = asset.size {
        anyhow::ensure!(
            bytes.len() == expected_size,
            "{} 下载不完整: expected={}, actual={}",
            asset.name,
            expected_size,
            bytes.len()
        );
    }
    let actual = sha256::digest(&bytes);
    anyhow::ensure!(
        actual == asset.sha256,
        "{} SHA-256 不匹配: expected={}, actual={actual}",
        asset.name,
        asset.sha256
    );
    Ok(bytes)
}

pub fn download_distribution_ko_to_file(
    distribution: KernelSuDistribution,
    kmi: &str,
    dst: impl AsRef<Path>,
) -> Result<String> {
    validate_release_component(kmi, "KMI", 32)?;
    let name = format!("{kmi}_kernelsu.ko");
    let origin = env::var("KSU_GITHUB_BASE_URL").unwrap_or_else(|_| GITHUB_ORIGIN.to_string());
    anyhow::ensure!(origin.starts_with("https://"), "GitHub 地址必须使用 HTTPS");

    let agent = build_http_agent()?;
    let metadata_url = format!(
        "{}/repos/{}/releases/latest",
        GITHUB_API,
        distribution.repository()
    );
    let origin = origin.trim_end_matches('/');
    let asset = request_bytes(
        &agent,
        &metadata_url,
        MAX_RELEASE_JSON_SIZE,
        true,
        "application/vnd.github+json",
    )
    .and_then(|(_, json)| parse_release_asset(distribution, &name, &json, origin))
    .or_else(|api_error| {
        log::warn!("GitHub API 不可用，回退到公开 release 页面: {api_error}");
        latest_release_asset_from_html(&agent, distribution, &name, origin)
    })?;
    let mut failures = Vec::new();
    let mut verified = None;
    for (url, authenticated) in download_candidates(&asset) {
        match request_bytes(
            &agent,
            &url,
            MAX_KERNEL_MODULE_SIZE,
            authenticated,
            "application/octet-stream",
        )
        .and_then(|(_, bytes)| validate_download(&asset, bytes))
        {
            Ok(bytes) => {
                verified = Some(bytes);
                break;
            }
            Err(error) => failures.push(format!("{url}: {error}")),
        }
    }
    let bytes = verified.ok_or_else(|| {
        anyhow::anyhow!(
            "{} 所有下载线路均失败:\n{}",
            asset.name,
            failures.join("\n")
        )
    })?;

    let dst = dst.as_ref();
    let parent = dst
        .parent()
        .ok_or_else(|| anyhow::anyhow!("目标路径缺少父目录: {}", dst.display()))?;
    fs::create_dir_all(parent).with_context(|| format!("创建 {} 失败", parent.display()))?;
    let temporary = parent.join(format!(".{}.download-{}", asset.name, std::process::id()));
    fs::write(&temporary, bytes).with_context(|| format!("写入 {} 失败", temporary.display()))?;
    if let Err(error) = fs::rename(&temporary, dst) {
        let _ = fs::remove_file(&temporary);
        return Err(error).with_context(|| format!("写入 {} 失败", dst.display()));
    }

    Ok(asset.tag)
}

#[cfg(target_os = "android")]
pub fn list_supported_kmi() -> std::vec::Vec<std::string::String> {
    Asset::iter()
        .filter_map(|file| file.strip_suffix("_kernelsu.ko").map(ToString::to_string))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traversal_kmi_is_rejected() {
        assert!(validate_release_component("../android14-6.1", "KMI", 32).is_err());
    }

    #[test]
    fn nul_kmi_is_rejected() {
        assert!(validate_release_component("android14-6.1\0evil", "KMI", 32).is_err());
    }

    #[test]
    fn oversized_kmi_is_rejected() {
        assert!(validate_release_component(&"A".repeat(1024), "KMI", 32).is_err());
    }

    #[test]
    fn truncated_release_json_is_rejected() {
        assert!(
            parse_release_asset(
                KernelSuDistribution::Official,
                "android14-6.1_kernelsu.ko",
                br#"{"tag_name":"v1","assets":["#,
                GITHUB_ORIGIN,
            )
            .is_err()
        );
    }

    #[test]
    fn oversized_release_json_is_rejected() {
        let json = vec![b' '; MAX_RELEASE_JSON_SIZE + 1];
        assert!(
            parse_release_asset(
                KernelSuDistribution::Official,
                "android14-6.1_kernelsu.ko",
                &json,
                GITHUB_ORIGIN,
            )
            .is_err()
        );
    }

    #[test]
    fn untrusted_download_url_is_rejected() {
        let json = br#"{
            "tag_name":"v1.0.0",
            "assets":[{
                "name":"android14-6.1_kernelsu.ko",
                "digest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "size":4096,
                "browser_download_url":"https://evil.invalid/module.ko"
            }]
        }"#;
        assert!(
            parse_release_asset(
                KernelSuDistribution::Official,
                "android14-6.1_kernelsu.ko",
                json,
                GITHUB_ORIGIN,
            )
            .is_err()
        );
    }

    #[test]
    fn release_kmis_are_sorted_and_deduplicated() {
        let json = br#"{
            "assets":[
                {"name":"android15-6.6_kernelsu.ko"},
                {"name":"android13-5.15_kernelsu.ko"},
                {"name":"android15-6.6_kernelsu.ko"},
                {"name":"manager.apk"}
            ]
        }"#;
        assert_eq!(
            parse_release_kmis(json).unwrap(),
            ["android13-5.15", "android15-6.6"]
        );
    }

    #[test]
    fn traversal_release_kmi_is_rejected() {
        let json = br#"{"assets":[{"name":"../android15-6.6_kernelsu.ko"}]}"#;
        assert!(parse_release_kmis(json).is_err());
    }

    #[test]
    fn empty_release_kmi_list_is_rejected() {
        assert!(parse_release_kmis(br#"{"assets":[]}"#).is_err());
    }

    #[test]
    fn oversized_release_kmi_list_is_rejected() {
        let assets = (0..513)
            .map(|index| format!(r#"{{"name":"android{index}-6.6_kernelsu.ko"}}"#))
            .collect::<Vec<_>>()
            .join(",");
        let json = format!(r#"{{"assets":[{assets}]}}"#);
        assert!(parse_release_kmis(json.as_bytes()).is_err());
    }

    #[test]
    #[ignore = "需要访问 GitHub"]
    fn live_downloads_only_requested_ko_from_every_distribution() {
        let directory = tempfile::tempdir().expect("创建测试目录失败");
        let distributions = [
            KernelSuDistribution::Official,
            KernelSuDistribution::SukiSuUltra,
            KernelSuDistribution::KernelSuNext,
            KernelSuDistribution::WildKsu,
        ];

        for distribution in distributions {
            let output = directory.path().join(format!("{}.ko", distribution.name()));
            let tag =
                download_distribution_ko_to_file(distribution, "android14-6.1", &output).unwrap();
            let size = fs::metadata(output).unwrap().len();
            assert!(!tag.is_empty(), "{}", distribution.name());
            assert!(size > 0 && size <= MAX_KERNEL_MODULE_SIZE as u64);
        }
    }

    #[test]
    #[ignore = "需要访问 GitHub"]
    fn live_fetches_current_kmis_from_every_distribution() {
        let distributions = [
            KernelSuDistribution::Official,
            KernelSuDistribution::SukiSuUltra,
            KernelSuDistribution::KernelSuNext,
            KernelSuDistribution::WildKsu,
        ];
        for distribution in distributions {
            let kmis = fetch_distribution_kmis(distribution).unwrap();
            assert!(!kmis.is_empty(), "{}", distribution.name());
            assert!(
                kmis.iter().all(|kmi| !kmi.contains("_kernelsu.ko")),
                "{}",
                distribution.name()
            );
        }
    }
}
