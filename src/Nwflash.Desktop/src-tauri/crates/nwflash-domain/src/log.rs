use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum LogLevel {
    Info,
    Success,
    Warning,
    Error,
}

impl LogLevel {
    pub fn display_label(&self) -> &'static str {
        match self {
            LogLevel::Info => "信息",
            LogLevel::Success => "完成",
            LogLevel::Warning => "注意",
            LogLevel::Error => "失败",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogEntry {
    pub timestamp_utc: i64,
    pub level: LogLevel,
    pub message: String,
    pub operation_id: Option<String>,
}
