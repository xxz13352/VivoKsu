use serde::{Deserialize, Serialize};

use crate::error::{DomainError, DomainResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum QuickFlashPartition {
    Boot,
    InitBoot,
    VendorBoot,
    Lk,
}

impl QuickFlashPartition {
    pub fn partition_name(&self) -> &'static str {
        match self {
            QuickFlashPartition::Boot => "boot",
            QuickFlashPartition::InitBoot => "init_boot",
            QuickFlashPartition::VendorBoot => "vendor_boot",
            QuickFlashPartition::Lk => "lk",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum FastbootTarget {
    Fastboot,
    Fastbootd,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlashImageInfo {
    pub path: String,
    pub size_bytes: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuickFlashRequest {
    pub partition: QuickFlashPartition,
    pub image: FlashImageInfo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuickFlashOptions {
    pub target: FastbootTarget,
    pub wait_for_device: bool,
    pub flash_both_slots: bool,
    pub switch_slot_after_flash: bool,
    pub auto_reboot: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuickFlashPlanItem {
    pub partition_name: String,
    pub image_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuickFlashExecutionPlan {
    pub requests: Vec<QuickFlashPlanItem>,
    pub switch_to_slot: Option<String>,
}

pub fn build_quick_flash_plan<F>(
    requests: &[QuickFlashRequest],
    options: &QuickFlashOptions,
    current_slot: Option<&str>,
    has_slot: F,
) -> DomainResult<QuickFlashExecutionPlan>
where
    F: Fn(&str) -> bool,
{
    if requests.is_empty() {
        return Err(DomainError::InvalidInput(
            "快速刷写至少需要选择一个镜像。".to_string(),
        ));
    }

    let mut plan: Vec<QuickFlashPlanItem> = Vec::with_capacity(requests.len() * 2);

    if options.flash_both_slots {
        for request in requests {
            let partition_name = request.partition.partition_name();
            let can_slot = has_slot(partition_name);
            if !can_slot {
                return Err(DomainError::InvalidOperation(format!(
                    "设备分区 {partition_name} 不支持 A/B 双槽刷写。"
                )));
            }

            plan.push(QuickFlashPlanItem {
                partition_name: format!("{partition_name}_a"),
                image_path: request.image.path.clone(),
            });
            plan.push(QuickFlashPlanItem {
                partition_name: format!("{partition_name}_b"),
                image_path: request.image.path.clone(),
            });
        }
    } else {
        for request in requests {
            plan.push(QuickFlashPlanItem {
                partition_name: request.partition.partition_name().to_string(),
                image_path: request.image.path.clone(),
            });
        }
    }

    let switch_to_slot = if options.flash_both_slots && options.switch_slot_after_flash {
        let active = normalize_to_slot(current_slot)?;
        Some(if active == "a" {
            "b".to_string()
        } else {
            "a".to_string()
        })
    } else {
        None
    };

    Ok(QuickFlashExecutionPlan {
        requests: plan,
        switch_to_slot,
    })
}

fn normalize_to_slot(value: Option<&str>) -> DomainResult<&'static str> {
    let Some(value) = value else {
        return Err(DomainError::InvalidOperation(
            "无法确定设备当前活动槽位。".to_string(),
        ));
    };

    // `fastboot getvar current-slot` returns `current-slot: a\nfinished. total
    // time: …`, not a bare letter; strip the `key:` prefix the way the WPF
    // `ExtractVariableValue` did before `NormalizeCurrentSlot`, then match the
    // bare slot letter.
    let slot = value
        .lines()
        .find_map(|line| line.rsplit_once(':').map(|(_, value)| value.trim()))
        .unwrap_or_else(|| value.trim());

    match slot.to_lowercase().as_str() {
        "a" | "_a" => Ok("a"),
        "b" | "_b" => Ok("b"),
        _ => Err(DomainError::InvalidOperation(
            "无法确定设备当前活动槽位。".to_string(),
        )),
    }
}
