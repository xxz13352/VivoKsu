use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DownloadProgress {
    pub downloaded_bytes: i64,
    pub total_bytes: Option<i64>,
    pub bytes_per_second: f64,
}
