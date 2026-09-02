use serde::{Deserialize, Serialize};

use crate::PartitionTaskSnapshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum OperationKind {
    Idle,
    Discovering,
    Rebooting,
    Installing,
    Transferring,
    Hashing,
    Flashing,
    Mirroring,
    Completed,
    Canceled,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OperationStateSnapshot {
    pub kind: OperationKind,
    pub operation_id: Option<String>,
    pub title: String,
    pub stage: String,
    pub progress: Option<f64>,
    pub started_at: Option<i64>,
    pub is_cancellable: bool,
    pub partition_task: Option<PartitionTaskSnapshot>,
    pub partition_tasks: Vec<PartitionTaskSnapshot>,
}

impl OperationStateSnapshot {
    pub fn idle() -> Self {
        Self {
            kind: OperationKind::Idle,
            operation_id: None,
            title: String::new(),
            stage: String::new(),
            progress: None,
            started_at: None,
            is_cancellable: false,
            partition_task: None,
            partition_tasks: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum OperationLogLevel {
    Info,
    Success,
    Warning,
    Error,
}

impl OperationLogLevel {
    pub fn display_label(&self) -> &'static str {
        match self {
            OperationLogLevel::Info => "信息",
            OperationLogLevel::Success => "完成",
            OperationLogLevel::Warning => "注意",
            OperationLogLevel::Error => "失败",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationLogEntry {
    pub timestamp_utc: i64,
    pub level: OperationLogLevel,
    pub message: String,
    pub operation_id: Option<String>,
}

impl OperationLogEntry {
    pub fn display_level(&self) -> &'static str {
        self.level.display_label()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageLogDetail {
    pub timestamp_utc: i64,
    pub level: OperationLogLevel,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageLogEntry {
    pub operation: String,
    pub title: String,
    pub status: String,
    pub event_id: String,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub duration_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub details: Vec<UsageLogDetail>,
}
