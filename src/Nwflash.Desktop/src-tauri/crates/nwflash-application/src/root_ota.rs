//! ROOT 云端 OTA 提取：从服务器解析出的 OTA 链接按需提取修补所需的启动分区镜像。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use nwflash_domain::{DomainError, FlashImageInfo};
use nwflash_infrastructure::remote_firmware::{
    extract_zip_members, list_zip_members, probe_remote_kind, RemoteFirmwareError,
    RemoteFirmwareKind,
};

use crate::FirmwareExtractService;

static ROOT_OTA_STAGING_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// 云提取选出的分区（boot/init_boot + vendor_boot）。
#[derive(Debug, Clone)]
pub struct RootOtaExtractedImages {
    /// boot 槽位镜像（init_boot 优先，否则 boot）。
    pub boot_image: Option<FlashImageInfo>,
    /// 实际选中的 boot 槽位分区名（`init_boot` 或 `boot`；无 boot 镜像时为空串）。
    pub boot_partition_name: String,
    /// vendor_boot 槽位镜像（可能缺失）。
    pub vendor_boot: Option<FlashImageInfo>,
    /// 提取 staging 根目录（由持有者负责清理）。
    pub staging_root: PathBuf,
}

pub struct RootOtaExtractOptions<'a> {
    pub url: &'a str,
    /// payload_dumper 可执行文件；仅 payload OTA 分支需要。
    pub payload_dumper: Option<&'a Path>,
    /// 提取输出的 staging 根目录。
    pub staging_root: &'a Path,
}

pub struct RootOtaService;

impl RootOtaService {
    pub fn new() -> Self {
        Self
    }

    /// 创建一个唯一、可写、由调用方负责清理的 staging 根目录。
    pub fn create_staging_root() -> Result<PathBuf, DomainError> {
        let unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or(0);
        let process = std::process::id();
        let sequence = ROOT_OTA_STAGING_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir()
            .join("nwflash-root-ota")
            .join(format!("{process}_{unix}_{sequence}"));
        std::fs::create_dir_all(&root).map_err(|error| {
            DomainError::InvalidOperation(format!("创建云提取临时目录失败：{error}"))
        })?;
        Ok(root)
    }

    /// 编排云提取：探测格式 → payload 或直接镜像 zip 分支 → 产出 boot/vendor_boot 镜像。
    #[allow(clippy::too_many_arguments)]
    pub fn extract<F, S, P>(
        &self,
        options: RootOtaExtractOptions<'_>,
        is_canceled: F,
        mut report_stage: S,
        mut report_progress: P,
    ) -> Result<RootOtaExtractedImages, DomainError>
    where
        F: FnMut() -> bool,
        S: FnMut(String),
        P: FnMut(f64),
    {
        let mut is_canceled = is_canceled;

        let kind = probe_remote_kind(options.url, None, &mut is_canceled)
            .map_err(|error| map_remote_firmware_error(error, "探测 OTA 格式失败。"))?;

        match kind {
            RemoteFirmwareKind::PayloadRaw | RemoteFirmwareKind::PayloadZip => {
                report_stage("正在读取 payload 分区信息".to_string());
                let executable = options.payload_dumper.ok_or_else(|| {
                    DomainError::ExternalTool("payload 提取工具未就绪。".to_string())
                })?;
                self.extract_from_payload(
                    executable,
                    options.url,
                    options.staging_root,
                    &mut is_canceled,
                    &mut report_progress,
                )
            }
            RemoteFirmwareKind::DirectImageZip => {
                report_stage("正在提取直接镜像 zip 分区".to_string());
                self.extract_from_direct_zip(
                    options.url,
                    options.staging_root,
                    &mut is_canceled,
                    &mut report_progress,
                )
            }
            RemoteFirmwareKind::Unsupported => Err(map_remote_firmware_error(
                RemoteFirmwareError::UnsupportedFormat,
                "该 OTA 格式无法云提取 ROOT 分区。",
            )),
        }
        .and_then(|images| {
            if images.boot_image.is_none() {
                return Err(RootOtaError::MissingBoot(
                    "该 OTA 不含可修补的 boot / init_boot 分区，请换一个 OTA 或选本地镜像。"
                        .to_string(),
                )
                .into_domain());
            }
            Ok(images)
        })
    }

    fn extract_from_payload<F, P>(
        &self,
        executable: &Path,
        url: &str,
        staging_root: &Path,
        is_canceled: &mut F,
        report_progress: &mut P,
    ) -> Result<RootOtaExtractedImages, DomainError>
    where
        F: FnMut() -> bool,
        P: FnMut(f64),
    {
        let metadata_directory = staging_root.join("metadata");
        let inspection =
            FirmwareExtractService::inspect_payload(executable, url, &metadata_directory, || {
                is_canceled()
            })
            .map_err(|error| map_firmware_extract_error(error, "读取 payload 分区信息失败。"))?;
        let _ = std::fs::remove_dir_all(&metadata_directory);
        if is_canceled() {
            return Err(RootOtaError::Cancelled.into_domain());
        }

        let boot_entry = pick_boot_entry(&inspection.entries);
        let vendor_boot_entry = pick_vendor_boot_entry(&inspection.entries);
        if boot_entry.is_none() && vendor_boot_entry.is_none() {
            return Err(RootOtaError::MissingBoot(
                "该 OTA 不含可修补的 boot / vendor_boot 分区。".to_string(),
            )
            .into_domain());
        }

        let mut selected: Vec<crate::FirmwareExtractEntry> = Vec::new();
        if let Some(entry) = &boot_entry {
            selected.push(entry.clone());
        }
        if let Some(entry) = &vendor_boot_entry {
            selected.push(entry.clone());
        }
        let total_bytes: u64 = selected
            .iter()
            .filter_map(|entry| u64::try_from(entry.size_bytes).ok())
            .sum();

        let image_directory = staging_root.join("images");
        let images = FirmwareExtractService::extract_payload_with_expected_sizes_and_progress(
            executable,
            url,
            &selected,
            &image_directory,
            || is_canceled(),
            |_partition, written| {
                if total_bytes > 0 {
                    report_progress((written as f64 / total_bytes as f64).clamp(0.0, 1.0));
                }
            },
        )
        .map_err(|error| map_firmware_extract_error(error, "提取 payload 分区失败。"))?;

        // extract 结果按 selected 顺序对应，文件名即 `{partition}.img`。
        let image_for = |entry: &crate::FirmwareExtractEntry| {
            images
                .iter()
                .find(|image| {
                    Path::new(&image.path)
                        .file_stem()
                        .and_then(|stem| stem.to_str())
                        .is_some_and(|stem| stem == entry.name)
                })
                .cloned()
        };

        let boot_image = boot_entry.as_ref().and_then(image_for);
        let vendor_boot_image = vendor_boot_entry.as_ref().and_then(image_for);
        let boot_partition_name = boot_entry
            .as_ref()
            .map(|entry| entry.name.clone())
            .unwrap_or_default();

        Ok(RootOtaExtractedImages {
            boot_image,
            boot_partition_name,
            vendor_boot: vendor_boot_image,
            staging_root: staging_root.to_path_buf(),
        })
    }

    fn extract_from_direct_zip<F, P>(
        &self,
        url: &str,
        staging_root: &Path,
        is_canceled: &mut F,
        report_progress: &mut P,
    ) -> Result<RootOtaExtractedImages, DomainError>
    where
        F: FnMut() -> bool,
        P: FnMut(f64),
    {
        let image_directory = staging_root.join("images");
        let wanted: [&str; 3] = ["init_boot", "boot", "vendor_boot"];

        // 先列出远程 zip 成员，算出目标分区的总字节，用于计算进度百分比。
        let members = list_zip_members(url, None, is_canceled)
            .map_err(|error| map_remote_firmware_error(error, "读取直接镜像 zip 分区列表失败。"))?;
        let total_bytes: u64 = members
            .iter()
            .filter(|member| wanted.contains(&member.name.as_str()))
            .map(|member| member.size_bytes.max(0) as u64)
            .sum();

        // 跨成员累计已下载字节。基础设施回调按成员报告当前值，应用层按差值合并。
        let mut completed_bytes = 0u64;
        let mut current_member: Option<String> = None;
        let mut current_member_bytes = 0u64;
        let mut progress_sink = |name: &str, bytes: u64| {
            if current_member.as_deref() != Some(name) {
                current_member = Some(name.to_string());
                current_member_bytes = 0;
            }
            let delta = bytes.saturating_sub(current_member_bytes);
            current_member_bytes = bytes;
            completed_bytes = completed_bytes.saturating_add(delta);
            if total_bytes > 0 {
                report_progress((completed_bytes as f64 / total_bytes as f64).clamp(0.0, 1.0));
            }
        };
        let extracted = extract_zip_members(
            url,
            None,
            &wanted,
            &image_directory,
            is_canceled,
            &mut progress_sink,
        )
        .map_err(|error| map_remote_firmware_error(error, "提取直接镜像 zip 分区失败。"))?;

        let boot = extracted
            .iter()
            .find(|image| image.partition_name == "init_boot")
            .or_else(|| {
                extracted
                    .iter()
                    .find(|image| image.partition_name == "boot")
            });
        let vendor_boot = extracted
            .iter()
            .find(|image| image.partition_name == "vendor_boot");

        let boot_partition_name = boot
            .map(|image| image.partition_name.clone())
            .unwrap_or_default();
        let to_flash =
            |image: &nwflash_infrastructure::remote_firmware::ExtractedZipImage| FlashImageInfo {
                path: image.output_path.clone(),
                size_bytes: image.size_bytes,
            };
        Ok(RootOtaExtractedImages {
            boot_image: boot.map(to_flash),
            boot_partition_name,
            vendor_boot: vendor_boot.map(to_flash),
            staging_root: staging_root.to_path_buf(),
        })
    }
}

#[derive(Debug)]
enum RootOtaError {
    InvalidFormat(String),
    MissingBoot(String),
    Cancelled,
    Remote(String),
}

impl RootOtaError {
    fn into_domain(self) -> DomainError {
        match self {
            RootOtaError::InvalidFormat(message) => DomainError::InvalidFormat(message),
            RootOtaError::MissingBoot(message) => DomainError::InvalidOperation(message),
            RootOtaError::Cancelled => DomainError::UserCancelled("云提取已取消。".to_string()),
            RootOtaError::Remote(message) => DomainError::InvalidOperation(message),
        }
    }
}

fn map_remote_firmware_error(error: RemoteFirmwareError, prefix: &str) -> DomainError {
    match error {
        RemoteFirmwareError::Cancelled => RootOtaError::Cancelled.into_domain(),
        RemoteFirmwareError::RangeUnsupported => {
            RootOtaError::Remote(format!("{prefix}服务器不支持 Range 请求。")).into_domain()
        }
        RemoteFirmwareError::UnsupportedFormat => {
            RootOtaError::InvalidFormat("不支持的 OTA 格式，无法云提取 ROOT 分区。".to_string())
                .into_domain()
        }
        RemoteFirmwareError::Archive(_) => {
            RootOtaError::InvalidFormat(format!("{prefix}OTA 压缩包无法读取或已损坏。"))
                .into_domain()
        }
        RemoteFirmwareError::Integrity(_) => {
            RootOtaError::InvalidFormat(format!("{prefix}OTA 分区完整性校验失败。")).into_domain()
        }
        RemoteFirmwareError::MissingPartition(_) => {
            RootOtaError::MissingBoot(format!("{prefix}OTA 不含所需启动分区。")).into_domain()
        }
        RemoteFirmwareError::Transport(_) => {
            RootOtaError::Remote(format!("{prefix}无法读取服务器 OTA，请重新检测后再试。"))
                .into_domain()
        }
        RemoteFirmwareError::InvalidUrl(_) => {
            RootOtaError::InvalidFormat(format!("{prefix}服务器 OTA 地址无效。")).into_domain()
        }
    }
}

fn map_firmware_extract_error(
    error: crate::FirmwareExtractApplicationError,
    prefix: &str,
) -> DomainError {
    if matches!(error, crate::FirmwareExtractApplicationError::Canceled) {
        RootOtaError::Cancelled.into_domain()
    } else {
        RootOtaError::InvalidFormat(format!("{prefix}{error}")).into_domain()
    }
}

fn pick_boot_entry(entries: &[crate::FirmwareExtractEntry]) -> Option<crate::FirmwareExtractEntry> {
    entries
        .iter()
        .find(|entry| entry.name.eq_ignore_ascii_case("init_boot"))
        .or_else(|| {
            entries
                .iter()
                .find(|entry| entry.name.eq_ignore_ascii_case("boot"))
        })
        .cloned()
}

fn pick_vendor_boot_entry(
    entries: &[crate::FirmwareExtractEntry],
) -> Option<crate::FirmwareExtractEntry> {
    entries
        .iter()
        .find(|entry| entry.name.eq_ignore_ascii_case("vendor_boot"))
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_errors_do_not_expose_ota_url_or_staging_path() {
        let error = map_remote_firmware_error(
            RemoteFirmwareError::Transport(
                "request https://example.invalid/private/ota.zip wrote C:\\secret\\staging"
                    .to_string(),
            ),
            "提取直接镜像 zip 分区失败。",
        );
        let message = error.to_string();
        assert!(!message.contains("https://"));
        assert!(!message.contains("C:\\secret"));
    }
}
