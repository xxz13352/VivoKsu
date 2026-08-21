use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek};

use serde::{Deserialize, Serialize};

use crate::error::{DomainError, DomainResult};

const HIGH_RISK_PREFIXES: [&str; 13] = [
    "abl",
    "frp",
    "gpt",
    "lk",
    "metadata",
    "modemst",
    "partition",
    "persist",
    "preloader",
    "super",
    "userdata",
    "vbmeta",
    "xbl",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum PartitionTransportKind {
    Automatic,
    AdbRoot,
    Fastboot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum PartitionOperationKind {
    Backup,
    Write,
    Erase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum PartitionTaskState {
    Waiting,
    Running,
    Succeeded,
    Failed,
    Canceled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PartitionTaskSnapshot {
    pub partition_name: String,
    pub state: PartitionTaskState,
    pub overall_progress: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DevicePartition {
    pub name: String,
    pub device_path: String,
    pub size_bytes: Option<i64>,
    pub slot: String,
    pub is_mounted: bool,
    pub is_high_risk: bool,
    pub can_backup: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartitionSnapshot {
    pub serial: String,
    pub transport: PartitionTransportKind,
    pub active_slot: String,
    pub partitions: Vec<DevicePartition>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartitionTask {
    pub partition_name: String,
    pub device_path: String,
    pub image_path: Option<String>,
    pub output_path: Option<String>,
    pub size_bytes: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartitionExecutionPlan {
    pub serial: String,
    pub transport: PartitionTransportKind,
    pub operation: PartitionOperationKind,
    pub tasks: Vec<PartitionTask>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PartitionTransferProgress {
    pub partition_name: String,
    pub transferred_bytes: i64,
    pub total_bytes: Option<i64>,
    pub bytes_per_second: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartitionOperationException {
    pub transport: PartitionTransportKind,
    pub partition_name: String,
    pub stage: String,
    pub message: String,
}

pub struct PartitionExecutionPlanBuilder;

impl Default for PartitionExecutionPlanBuilder {
    fn default() -> Self {
        Self
    }
}

impl PartitionExecutionPlanBuilder {
    pub fn build_write(
        &self,
        serial: &str,
        transport: PartitionTransportKind,
        selected_partitions: &[DevicePartition],
        image_paths: &HashMap<String, String>,
    ) -> DomainResult<PartitionExecutionPlan> {
        let tasks: Vec<PartitionTask> = selected_partitions
            .iter()
            .filter_map(|partition| {
                let image_path = image_paths.get(&partition.name)?;
                if image_path.trim().is_empty() {
                    return None;
                }

                Some(PartitionTask {
                    partition_name: partition.name.clone(),
                    device_path: partition.device_path.clone(),
                    image_path: Some(image_path.clone()),
                    output_path: None,
                    size_bytes: partition.size_bytes,
                })
            })
            .collect();

        if matches!(transport, PartitionTransportKind::AdbRoot) {
            validate_adb_root_write_tasks(&tasks)?;
        }

        create_plan(serial, transport, PartitionOperationKind::Write, tasks)
    }

    pub fn build_backup(
        &self,
        serial: &str,
        transport: PartitionTransportKind,
        selected_partitions: &[DevicePartition],
        output_directory: &str,
    ) -> DomainResult<PartitionExecutionPlan> {
        if output_directory.trim().is_empty() {
            return Err(DomainError::InvalidInput("输出目录不能为空。".to_string()));
        }

        let tasks: Vec<PartitionTask> = selected_partitions
            .iter()
            .map(|partition| PartitionTask {
                partition_name: partition.name.clone(),
                device_path: partition.device_path.clone(),
                image_path: None,
                output_path: Some(format!(
                    "{}\\{}.img",
                    output_directory.trim_end_matches('\\'),
                    partition.name
                )),
                size_bytes: partition.size_bytes,
            })
            .collect();

        create_plan(serial, transport, PartitionOperationKind::Backup, tasks)
    }

    pub fn build_erase(
        &self,
        serial: &str,
        transport: PartitionTransportKind,
        selected_partitions: &[DevicePartition],
    ) -> DomainResult<PartitionExecutionPlan> {
        let tasks: Vec<PartitionTask> = selected_partitions
            .iter()
            .map(|partition| PartitionTask {
                partition_name: partition.name.clone(),
                device_path: partition.device_path.clone(),
                image_path: None,
                output_path: None,
                size_bytes: partition.size_bytes,
            })
            .collect();

        create_plan(serial, transport, PartitionOperationKind::Erase, tasks)
    }
}

fn create_plan(
    serial: &str,
    transport: PartitionTransportKind,
    operation: PartitionOperationKind,
    tasks: Vec<PartitionTask>,
) -> DomainResult<PartitionExecutionPlan> {
    if serial.trim().is_empty() {
        return Err(DomainError::InvalidInput(
            "执行计划缺少设备序列号。".to_string(),
        ));
    }

    if matches!(transport, PartitionTransportKind::Automatic) {
        return Err(DomainError::InvalidInput(
            "执行计划必须使用已解析的设备通道。".to_string(),
        ));
    }

    if tasks.is_empty() {
        return Err(DomainError::InvalidOperation(
            "请至少选择一个可执行分区任务。".to_string(),
        ));
    }

    Ok(PartitionExecutionPlan {
        serial: serial.to_string(),
        transport,
        operation,
        tasks,
    })
}

fn validate_adb_root_write_tasks(tasks: &[PartitionTask]) -> DomainResult<()> {
    for task in tasks {
        let image_path = task
            .image_path
            .as_ref()
            .ok_or_else(|| DomainError::InvalidOperation("分区镜像路径缺失。".to_string()))?;

        let metadata = std::fs::metadata(image_path).map_err(|_| {
            DomainError::InvalidOperation(format!(
                "分区 {} 的镜像文件不存在：{}",
                task.partition_name, image_path
            ))
        })?;

        let image_length = i64::try_from(metadata.len()).unwrap_or(i64::MAX);
        if image_length == 0 {
            return Err(DomainError::InvalidOperation(format!(
                "分区 {} 的镜像文件为空：{}",
                task.partition_name, image_path
            )));
        }

        if let Some(size_limit) = task.size_bytes {
            if image_length > size_limit {
                return Err(DomainError::InvalidOperation(format!(
                    "分区 {} 的镜像文件大于分区容量。",
                    task.partition_name
                )));
            }
        }

        let header = {
            let mut header = [0u8; 4];
            let mut file = File::open(image_path).map_err(|_| {
                DomainError::InvalidOperation(format!(
                    "分区 {} 的镜像文件不存在：{}",
                    task.partition_name, image_path
                ))
            })?;
            let read = file.read(&mut header).map_err(|_| {
                DomainError::InvalidOperation(format!(
                    "分区 {} 的镜像读取失败：{}",
                    task.partition_name, image_path
                ))
            })?;
            let _ = file.seek(std::io::SeekFrom::Start(0));
            (header, read)
        };

        if header.1 == 4 && header.0 == [0x3A, 0xFF, 0x26, 0xED] {
            return Err(DomainError::InvalidOperation(format!(
                "分区 {} 的镜像是 Android sparse 镜像，不能通过 ADB Root 直接写入。",
                task.partition_name
            )));
        }
    }

    Ok(())
}

pub fn is_high_risk_partition(partition_name: &str) -> bool {
    let name = partition_name.to_lowercase();
    HIGH_RISK_PREFIXES.iter().any(|prefix| {
        if name.len() < prefix.len() || !name.starts_with(prefix) {
            return false;
        }

        if name.len() == prefix.len() {
            return true;
        }

        let next = name.as_bytes()[prefix.len()];
        next == b'_' || next.is_ascii_digit()
    })
}

pub fn format_partition_size(bytes: i64) -> String {
    const KILOBYTE: i64 = 1024;
    const MEGABYTE: i64 = KILOBYTE * 1024;
    const GIGABYTE: i64 = MEGABYTE * 1024;

    if bytes >= GIGABYTE {
        compact_one_decimal(bytes as f64 / GIGABYTE as f64) + " GB"
    } else if bytes >= MEGABYTE {
        compact_one_decimal(bytes as f64 / MEGABYTE as f64) + " MB"
    } else if bytes >= KILOBYTE {
        compact_one_decimal(bytes as f64 / KILOBYTE as f64) + " KB"
    } else {
        format!("{bytes} B")
    }
}

fn compact_one_decimal(value: f64) -> String {
    let text = format!("{value:.1}");
    match text.strip_suffix(".0") {
        Some(trimmed) => trimmed.to_string(),
        None => text,
    }
}
