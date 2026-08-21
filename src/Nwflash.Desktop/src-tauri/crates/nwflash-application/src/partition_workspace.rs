//! Explicit Fastboot partition-table parsing for the partition workspace.

use std::{collections::HashMap, path::Path};

use nwflash_domain::{
    is_high_risk_partition, DevicePartition, DomainError, FlashImageInfo, PartitionExecutionPlan,
    PartitionExecutionPlanBuilder, PartitionSnapshot, PartitionTransportKind,
};

#[derive(Debug, Default)]
pub struct PartitionWorkspace {
    snapshot: Option<PartitionSnapshot>,
    image_paths: HashMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PartitionSelectionSummary {
    pub task_count: usize,
    pub high_risk_count: usize,
    pub mounted_count: usize,
}

impl PartitionWorkspace {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn apply_snapshot(&mut self, snapshot: PartitionSnapshot) {
        self.snapshot = Some(snapshot);
    }

    pub fn cached_snapshot(&self) -> Option<PartitionSnapshot> {
        self.snapshot.clone()
    }

    pub fn map_images(&mut self, images: &[FlashImageInfo]) -> Vec<String> {
        let Some(snapshot) = self.snapshot.as_ref() else {
            return Vec::new();
        };
        let mut mapped = Vec::new();

        for image in images {
            let Some(base_name) = Path::new(&image.path)
                .file_stem()
                .and_then(|value| value.to_str())
            else {
                continue;
            };
            let partition = snapshot
                .partitions
                .iter()
                .find(|partition| partition.name.eq_ignore_ascii_case(base_name))
                .or_else(|| {
                    matches!(snapshot.active_slot.as_str(), "a" | "b")
                        .then(|| format!("{base_name}_{}", snapshot.active_slot))
                        .and_then(|slot_name| {
                            snapshot
                                .partitions
                                .iter()
                                .find(|partition| partition.name.eq_ignore_ascii_case(&slot_name))
                        })
                });

            if let Some(partition) = partition {
                self.image_paths
                    .insert(partition.name.clone(), image.path.clone());
                mapped.push(partition.name.clone());
            }
        }

        mapped
    }

    pub fn build_erase_plan(
        &self,
        selected_names: &[String],
    ) -> Result<PartitionExecutionPlan, DomainError> {
        let snapshot = self.snapshot()?;
        let selected = self.resolve_selected(snapshot, selected_names)?;
        PartitionExecutionPlanBuilder.build_erase(&snapshot.serial, snapshot.transport, &selected)
    }

    pub fn selection_summary(
        &self,
        selected_names: &[String],
    ) -> Result<PartitionSelectionSummary, DomainError> {
        let snapshot = self.snapshot()?;
        let selected = self.resolve_selected(snapshot, selected_names)?;
        Ok(PartitionSelectionSummary {
            task_count: selected.len(),
            high_risk_count: selected
                .iter()
                .filter(|partition| partition.is_high_risk)
                .count(),
            mounted_count: selected
                .iter()
                .filter(|partition| partition.is_mounted)
                .count(),
        })
    }

    pub fn build_write_plan(
        &self,
        selected_names: &[String],
    ) -> Result<PartitionExecutionPlan, DomainError> {
        let snapshot = self.snapshot()?;
        let selected = self.resolve_selected(snapshot, selected_names)?;
        PartitionExecutionPlanBuilder.build_write(
            &snapshot.serial,
            snapshot.transport,
            &selected,
            &self.image_paths,
        )
    }

    pub fn build_backup_plan(
        &self,
        selected_names: &[String],
        output_directory: &str,
    ) -> Result<PartitionExecutionPlan, DomainError> {
        let snapshot = self.snapshot()?;
        if matches!(snapshot.transport, PartitionTransportKind::Fastboot) {
            return Err(DomainError::InvalidOperation(
                "Fastboot 模式不支持备份/回读分区，请切换到 ADB Root 通道。".to_string(),
            ));
        }
        let selected = self.resolve_selected(snapshot, selected_names)?;
        PartitionExecutionPlanBuilder.build_backup(
            &snapshot.serial,
            snapshot.transport,
            &selected,
            output_directory,
        )
    }

    fn snapshot(&self) -> Result<&PartitionSnapshot, DomainError> {
        self.snapshot
            .as_ref()
            .ok_or_else(|| DomainError::InvalidOperation("请先读取分区表。".to_string()))
    }

    fn resolve_selected(
        &self,
        snapshot: &PartitionSnapshot,
        selected_names: &[String],
    ) -> Result<Vec<DevicePartition>, DomainError> {
        let mut selected = Vec::with_capacity(selected_names.len());
        for name in selected_names {
            let partition = snapshot
                .partitions
                .iter()
                .find(|partition| partition.name.eq_ignore_ascii_case(name))
                .ok_or_else(|| DomainError::InvalidInput(format!("分区表中不存在分区：{name}")))?;
            selected.push(partition.clone());
        }
        Ok(selected)
    }
}

pub fn parse_fastboot_partition_table(
    serial: &str,
    output: &str,
) -> Result<PartitionSnapshot, DomainError> {
    if serial.trim().is_empty() {
        return Err(DomainError::InvalidInput(
            "设备序列号不能为空。".to_string(),
        ));
    }

    let mut active_slot = String::new();
    let mut partitions = Vec::new();
    for line in output.lines().map(normalize_line) {
        if let Some(slot) = line.strip_prefix("current-slot:") {
            active_slot = slot.trim().trim_start_matches('_').to_ascii_lowercase();
            continue;
        }
        let Some(value) = line.strip_prefix("partition-size:") else {
            continue;
        };
        let Some((name, size)) = value.rsplit_once(':') else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        partitions.push(DevicePartition {
            name: name.to_string(),
            device_path: name.to_string(),
            size_bytes: parse_partition_size(size),
            slot: partition_slot(name),
            is_mounted: false,
            is_high_risk: is_high_risk_partition(name),
            can_backup: true,
        });
    }
    partitions.sort_by(|left, right| {
        left.name
            .to_ascii_lowercase()
            .cmp(&right.name.to_ascii_lowercase())
    });
    Ok(PartitionSnapshot {
        serial: serial.to_string(),
        transport: PartitionTransportKind::Fastboot,
        active_slot,
        partitions,
    })
}

pub fn parse_adb_root_partition_table(
    serial: &str,
    active_slot: &str,
    output: &str,
) -> Result<PartitionSnapshot, DomainError> {
    if serial.trim().is_empty() {
        return Err(DomainError::InvalidInput(
            "设备序列号不能为空。".to_string(),
        ));
    }

    let mut partitions = HashMap::new();
    for line in output.lines() {
        let fields: Vec<_> = line.split('|').map(str::trim).collect();
        if fields.len() != 4 || fields[0].is_empty() || fields[1].is_empty() {
            continue;
        }
        let name = fields[0];
        partitions
            .entry(name.to_ascii_lowercase())
            .or_insert_with(|| DevicePartition {
                name: name.to_string(),
                device_path: fields[1].to_string(),
                size_bytes: fields[2].parse::<i64>().ok(),
                slot: partition_slot(name),
                is_mounted: fields[3] == "1",
                is_high_risk: is_high_risk_partition(name),
                can_backup: true,
            });
    }

    let mut partitions: Vec<_> = partitions.into_values().collect();
    partitions.sort_by(|left, right| {
        left.name
            .to_ascii_lowercase()
            .cmp(&right.name.to_ascii_lowercase())
    });
    Ok(PartitionSnapshot {
        serial: serial.to_string(),
        transport: PartitionTransportKind::AdbRoot,
        active_slot: active_slot
            .trim()
            .trim_start_matches('_')
            .to_ascii_lowercase(),
        partitions,
    })
}

fn normalize_line(source: &str) -> &str {
    let line = source.trim();
    let line = line
        .strip_prefix("(bootloader)")
        .unwrap_or(line)
        .trim_start();
    line.strip_prefix("INFO").unwrap_or(line).trim_start()
}

fn parse_partition_size(value: &str) -> Option<i64> {
    let value = value.trim();
    if let Some(hexadecimal) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        return i64::from_str_radix(hexadecimal, 16).ok();
    }
    value.parse::<i64>().ok()
}

fn partition_slot(name: &str) -> String {
    if name.ends_with("_a") {
        "a".to_string()
    } else if name.ends_with("_b") {
        "b".to_string()
    } else {
        String::new()
    }
}
