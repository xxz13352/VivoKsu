use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RomInfo {
    pub pd: String,
    pub version: String,
    pub url: String,
    pub name: Option<String>,
    pub size_bytes: Option<i64>,
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirmwarePackageInspection {
    pub package_path: String,
    pub package_name: String,
    pub entry_count: usize,
    pub image_entries: Vec<String>,
}

impl FirmwarePackageInspection {
    pub fn managed_image_entries(&self) -> Vec<String> {
        const MANAGED: [&str; 4] = ["boot", "init_boot", "vendor_boot", "lk"];

        self.image_entries
            .iter()
            .filter_map(|entry| {
                let name = std::path::Path::new(entry)
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default();

                if MANAGED
                    .iter()
                    .any(|allowed| allowed.eq_ignore_ascii_case(name))
                {
                    Some(entry.clone())
                } else {
                    None
                }
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PayloadPartitionEntry {
    pub name: String,
    pub size_bytes: i64,
    pub compression_type: String,
}

impl PayloadPartitionEntry {
    pub fn size_text(&self) -> String {
        if self.size_bytes < 1024 {
            format!("{} B", self.size_bytes)
        } else if self.size_bytes < 1024 * 1024 {
            format!("{:.1} KB", self.size_bytes as f64 / 1024f64)
        } else if self.size_bytes < 1024 * 1024 * 1024 {
            format!("{:.1} MB", self.size_bytes as f64 / 1024f64 / 1024f64)
        } else {
            format!(
                "{:.2} GB",
                self.size_bytes as f64 / 1024f64 / 1024f64 / 1024f64
            )
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PayloadExtractionResult {
    pub partition_name: String,
    pub output_path: String,
    pub size_bytes: i64,
}
