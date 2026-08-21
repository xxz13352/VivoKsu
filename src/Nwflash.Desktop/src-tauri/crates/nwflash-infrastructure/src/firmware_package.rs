use std::{fs::File, path::Path};

use nwflash_domain::FirmwarePackageInspection;
use thiserror::Error;
use zip::ZipArchive;

#[derive(Debug, Error)]
pub enum FirmwarePackageError {
    #[error("未找到固件包。")]
    NotFound,
    #[error("当前仅支持检查 ZIP 格式的 Vivo 固件包。")]
    UnsupportedFormat,
    #[error("无法读取固件包：{0}")]
    Read(String),
}

pub struct FirmwarePackageInspector;

impl FirmwarePackageInspector {
    pub fn contains_payload_bin(package_path: &Path) -> Result<bool, FirmwarePackageError> {
        if !package_path.is_file() {
            return Err(FirmwarePackageError::NotFound);
        }
        if !package_path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"))
        {
            return Err(FirmwarePackageError::UnsupportedFormat);
        }

        let file = File::open(package_path)
            .map_err(|error| FirmwarePackageError::Read(error.to_string()))?;
        let mut archive =
            ZipArchive::new(file).map_err(|error| FirmwarePackageError::Read(error.to_string()))?;
        for index in 0..archive.len() {
            let entry = archive
                .by_index(index)
                .map_err(|error| FirmwarePackageError::Read(error.to_string()))?;
            if Path::new(entry.name())
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.eq_ignore_ascii_case("payload.bin"))
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn inspect(package_path: &Path) -> Result<FirmwarePackageInspection, FirmwarePackageError> {
        if !package_path.is_file() {
            return Err(FirmwarePackageError::NotFound);
        }
        if !package_path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"))
        {
            return Err(FirmwarePackageError::UnsupportedFormat);
        }

        let file = File::open(package_path)
            .map_err(|error| FirmwarePackageError::Read(error.to_string()))?;
        let mut archive =
            ZipArchive::new(file).map_err(|error| FirmwarePackageError::Read(error.to_string()))?;
        let entry_count = archive.len();
        let mut image_entries = Vec::new();

        for index in 0..entry_count {
            let entry = archive
                .by_index(index)
                .map_err(|error| FirmwarePackageError::Read(error.to_string()))?;
            let name = entry.name();
            if !name.is_empty() && name.to_ascii_lowercase().ends_with(".img") {
                image_entries.push(name.to_string());
            }
        }
        image_entries.sort_by_key(|entry| entry.to_ascii_lowercase());

        Ok(FirmwarePackageInspection {
            package_path: package_path.to_string_lossy().into_owned(),
            package_name: package_path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_string(),
            entry_count,
            image_entries,
        })
    }
}
