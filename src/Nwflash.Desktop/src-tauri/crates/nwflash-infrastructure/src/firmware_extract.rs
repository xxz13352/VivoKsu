use std::{
    fs::{self, File},
    io::{Read, Write},
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use nwflash_domain::{FirmwarePackageInspection, FlashImageInfo, QuickFlashPartition};
use reqwest::{header, StatusCode};
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use zip::ZipArchive;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirmwareFormat {
    ImageDirectory,
    VivoGzipTar,
    Zip,
    Payload,
    Unknown,
}

pub struct FirmwareFormatDetector;

impl FirmwareFormatDetector {
    pub fn detect_local(path: &Path) -> Result<FirmwareFormat, FirmwareExtractionError> {
        if path.is_dir() {
            return Ok(FirmwareFormat::ImageDirectory);
        }
        if !path.is_file() {
            return Err(FirmwareExtractionError::Io("未找到固件源。".to_string()));
        }

        let mut prefix = [0u8; 4];
        let mut source =
            File::open(path).map_err(|error| FirmwareExtractionError::Io(error.to_string()))?;
        let prefix_length = source
            .read(&mut prefix)
            .map_err(|error| FirmwareExtractionError::Io(error.to_string()))?;
        let prefix = &prefix[..prefix_length];

        Ok(
            if prefix.starts_with(&[0x1f, 0x8b]) || prefix == [0x28, 0xb5, 0x2f, 0xfd] {
                FirmwareFormat::VivoGzipTar
            } else if prefix.starts_with(b"PK") {
                FirmwareFormat::Zip
            } else if prefix.starts_with(b"CrAU") {
                FirmwareFormat::Payload
            } else {
                FirmwareFormat::Unknown
            },
        )
    }

    /// Reads only the Android payload magic through a mandatory four-byte HTTP range.
    pub async fn detect_remote_payload(
        url: &str,
    ) -> Result<FirmwareFormat, FirmwareExtractionError> {
        Self::detect_remote_payload_with_cancel(url, &CancellationToken::new()).await
    }

    pub async fn detect_remote_payload_with_cancel(
        url: &str,
        cancellation: &CancellationToken,
    ) -> Result<FirmwareFormat, FirmwareExtractionError> {
        let request = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|error| FirmwareExtractionError::Io(format!("远程固件读取失败：{error}")))?
            .get(url)
            .header(header::RANGE, "bytes=0-3")
            .send();
        let response = tokio::select! {
            _ = cancellation.cancelled() => return Err(FirmwareExtractionError::Canceled),
            response = request => response,
        }
        .map_err(|error| FirmwareExtractionError::Io(format!("远程固件读取失败：{error}")))?;
        let response = response
            .error_for_status()
            .map_err(|error| FirmwareExtractionError::Io(format!("远程固件读取失败：{error}")))?;
        if response.status() != StatusCode::PARTIAL_CONTENT
            || response.content_length() != Some(4)
            || response
                .headers()
                .get(header::CONTENT_RANGE)
                .and_then(|value| value.to_str().ok())
                .and_then(valid_payload_magic_content_range)
                .is_none()
        {
            return Err(FirmwareExtractionError::Io(
                "远程固件 Range 响应无效。".to_string(),
            ));
        }
        let bytes = response.bytes();
        let prefix = tokio::select! {
            _ = cancellation.cancelled() => return Err(FirmwareExtractionError::Canceled),
            bytes = bytes => bytes,
        }
        .map_err(|error| FirmwareExtractionError::Io(format!("远程固件读取失败：{error}")))?;
        Ok(if prefix.as_ref() == b"CrAU" {
            FirmwareFormat::Payload
        } else {
            FirmwareFormat::Unknown
        })
    }
}

fn valid_payload_magic_content_range(value: &str) -> Option<()> {
    let total = value.strip_prefix("bytes 0-3/")?.parse::<u64>().ok()?;
    (total >= 4).then_some(())
}

#[derive(Debug, Error)]
pub enum FirmwareExtractionError {
    #[error("固件提取已取消。")]
    Canceled,
    #[error("该镜像不在受控的快速刷写分区范围内。")]
    UnmanagedImage,
    #[error("固件包中未找到指定镜像。")]
    EntryNotFound,
    #[error("提取出的镜像为空。")]
    EmptyImage,
    #[error("固件镜像提取失败：{0}")]
    Io(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirmwarePackageExtractionResult {
    pub image: FlashImageInfo,
    pub partition: QuickFlashPartition,
}

pub struct FirmwarePackageExtractionService;

impl FirmwarePackageExtractionService {
    pub fn extract(
        package: &FirmwarePackageInspection,
        entry_path: &str,
        staging_root: &Path,
    ) -> Result<FirmwarePackageExtractionResult, FirmwareExtractionError> {
        Self::extract_with_cancel(package, entry_path, staging_root, || false)
    }

    pub fn extract_with_cancel<F>(
        package: &FirmwarePackageInspection,
        entry_path: &str,
        staging_root: &Path,
        mut is_canceled: F,
    ) -> Result<FirmwarePackageExtractionResult, FirmwareExtractionError>
    where
        F: FnMut() -> bool,
    {
        let entry_path = package
            .managed_image_entries()
            .into_iter()
            .find(|entry| entry.eq_ignore_ascii_case(entry_path))
            .ok_or(FirmwareExtractionError::UnmanagedImage)?;
        let partition = partition_from_entry(&entry_path)?;

        fs::create_dir_all(staging_root)
            .map_err(|error| FirmwareExtractionError::Io(error.to_string()))?;
        let output_path = staging_root.join(format!(
            "{}-{}.img",
            partition.partition_name(),
            unique_suffix()
        ));

        let result = (|| {
            ensure_not_canceled(&mut is_canceled)?;
            let package_file = File::open(&package.package_path)
                .map_err(|error| FirmwareExtractionError::Io(error.to_string()))?;
            let mut archive = ZipArchive::new(package_file)
                .map_err(|error| FirmwareExtractionError::Io(error.to_string()))?;
            let entry_index = (0..archive.len())
                .find(|index| {
                    archive
                        .by_index(*index)
                        .map(|entry| entry.name().eq_ignore_ascii_case(&entry_path))
                        .unwrap_or(false)
                })
                .ok_or(FirmwareExtractionError::EntryNotFound)?;
            let mut entry = archive
                .by_index(entry_index)
                .map_err(|error| FirmwareExtractionError::Io(error.to_string()))?;
            let mut output = File::create_new(&output_path)
                .map_err(|error| FirmwareExtractionError::Io(error.to_string()))?;
            copy_entry_with_cancel(&mut entry, &mut output, &mut is_canceled)?;
            output
                .sync_all()
                .map_err(|error| FirmwareExtractionError::Io(error.to_string()))?;

            let size_bytes = i64::try_from(
                fs::metadata(&output_path)
                    .map_err(|error| FirmwareExtractionError::Io(error.to_string()))?
                    .len(),
            )
            .unwrap_or(i64::MAX);
            if size_bytes == 0 {
                return Err(FirmwareExtractionError::EmptyImage);
            }

            Ok(FirmwarePackageExtractionResult {
                image: FlashImageInfo {
                    path: output_path.to_string_lossy().into_owned(),
                    size_bytes,
                },
                partition,
            })
        })();

        if result.is_err() {
            let _ = fs::remove_file(&output_path);
        }
        result
    }

    pub fn export_image_to_directory_with_cancel<F>(
        package: &FirmwarePackageInspection,
        entry_path: &str,
        output_directory: &Path,
        mut is_canceled: F,
    ) -> Result<FlashImageInfo, FirmwareExtractionError>
    where
        F: FnMut() -> bool,
    {
        let entry_path = package
            .image_entries
            .iter()
            .find(|entry| entry.eq_ignore_ascii_case(entry_path))
            .cloned()
            .ok_or(FirmwareExtractionError::EntryNotFound)?;
        let file_name = Path::new(&entry_path)
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .ok_or(FirmwareExtractionError::EntryNotFound)?;
        fs::create_dir_all(output_directory)
            .map_err(|error| FirmwareExtractionError::Io(error.to_string()))?;
        let output_path = output_directory.join(file_name);
        let partial_path =
            output_directory.join(format!(".{file_name}.partial-{}", unique_suffix()));

        let result = (|| {
            ensure_not_canceled(&mut is_canceled)?;
            let package_file = File::open(&package.package_path)
                .map_err(|error| FirmwareExtractionError::Io(error.to_string()))?;
            let mut archive = ZipArchive::new(package_file)
                .map_err(|error| FirmwareExtractionError::Io(error.to_string()))?;
            let entry_index = (0..archive.len())
                .find(|index| {
                    archive
                        .by_index(*index)
                        .map(|entry| entry.name().eq_ignore_ascii_case(&entry_path))
                        .unwrap_or(false)
                })
                .ok_or(FirmwareExtractionError::EntryNotFound)?;
            let mut entry = archive
                .by_index(entry_index)
                .map_err(|error| FirmwareExtractionError::Io(error.to_string()))?;
            let mut partial = File::create_new(&partial_path)
                .map_err(|error| FirmwareExtractionError::Io(error.to_string()))?;
            copy_entry_with_cancel(&mut entry, &mut partial, &mut is_canceled)?;
            partial
                .sync_all()
                .map_err(|error| FirmwareExtractionError::Io(error.to_string()))?;
            let size_bytes = i64::try_from(
                fs::metadata(&partial_path)
                    .map_err(|error| FirmwareExtractionError::Io(error.to_string()))?
                    .len(),
            )
            .unwrap_or(i64::MAX);
            if size_bytes == 0 {
                return Err(FirmwareExtractionError::EmptyImage);
            }
            fs::rename(&partial_path, &output_path)
                .map_err(|error| FirmwareExtractionError::Io(error.to_string()))?;
            Ok(FlashImageInfo {
                path: output_path.to_string_lossy().into_owned(),
                size_bytes,
            })
        })();
        if result.is_err() {
            let _ = fs::remove_file(&partial_path);
        }
        result
    }
}

fn copy_entry_with_cancel(
    source: &mut impl Read,
    destination: &mut impl Write,
    is_canceled: &mut impl FnMut() -> bool,
) -> Result<(), FirmwareExtractionError> {
    let mut buffer = [0u8; 8192];
    loop {
        ensure_not_canceled(is_canceled)?;
        let count = source
            .read(&mut buffer)
            .map_err(|error| FirmwareExtractionError::Io(error.to_string()))?;
        if count == 0 {
            return Ok(());
        }
        destination
            .write_all(&buffer[..count])
            .map_err(|error| FirmwareExtractionError::Io(error.to_string()))?;
    }
}

fn ensure_not_canceled(
    is_canceled: &mut impl FnMut() -> bool,
) -> Result<(), FirmwareExtractionError> {
    if is_canceled() {
        return Err(FirmwareExtractionError::Canceled);
    }
    Ok(())
}

fn partition_from_entry(entry_path: &str) -> Result<QuickFlashPartition, FirmwareExtractionError> {
    match Path::new(entry_path)
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "boot" => Ok(QuickFlashPartition::Boot),
        "init_boot" => Ok(QuickFlashPartition::InitBoot),
        "vendor_boot" => Ok(QuickFlashPartition::VendorBoot),
        "lk" => Ok(QuickFlashPartition::Lk),
        _ => Err(FirmwareExtractionError::UnmanagedImage),
    }
}

fn unique_suffix() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}
