use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum SafeFlashSlotMode {
    CurrentSlot,
    OtherSlot,
    BothSlots,
}

pub fn is_slot_based_mode(mode: SafeFlashSlotMode) -> bool {
    matches!(
        mode,
        SafeFlashSlotMode::OtherSlot | SafeFlashSlotMode::BothSlots
    )
}

pub fn other_slot(current_slot: Option<&str>) -> Option<&'static str> {
    match current_slot
        .unwrap_or_default()
        .trim()
        .to_lowercase()
        .as_str()
    {
        "a" | "_a" => Some("b"),
        "b" | "_b" => Some("a"),
        _ => None,
    }
}

pub fn compute_targets(
    partition_name: &str,
    mode: SafeFlashSlotMode,
    current_slot: Option<&str>,
    has_slot: bool,
) -> Vec<String> {
    if !has_slot {
        return vec![partition_name.to_string()];
    }

    match mode {
        SafeFlashSlotMode::CurrentSlot => vec![partition_name.to_string()],
        // current-slot 读不到时回退分区原名（C# SafeFlashSlotPlanner 的
        // “回退原样刷写，安全降级不砖机”决策）：瞬态 getvar 失败绝不能让
        // 整次刷写丢目标。
        SafeFlashSlotMode::OtherSlot => {
            vec![append_slot(partition_name, other_slot(current_slot))]
        }
        SafeFlashSlotMode::BothSlots => vec![
            append_slot(partition_name, Some("a")),
            append_slot(partition_name, Some("b")),
        ],
    }
}

fn append_slot(partition_name: &str, slot: Option<&str>) -> String {
    slot.map(|it| format!("{partition_name}_{it}"))
        .unwrap_or_else(|| partition_name.to_string())
}

/// `lk` 与 `preloader` 都是引导加载分区：安全刷写模式下不仅基名要跳过，
/// 带槽位后缀的变体（`lk_a`/`lk_b`）同样必须跳过，与 preloader 的子串
/// 匹配语义对齐。lk 用 前缀+`_`/数字 边界判定，避免误伤 `lksec` 等普通分区。
pub fn should_skip_safe_flash_partition(partition_name: &str) -> bool {
    let name = partition_name.to_lowercase();
    name == "lk"
        || name
            .strip_prefix("lk")
            .is_some_and(|rest| rest.starts_with('_') || rest.starts_with(|c: char| c.is_ascii_digit()))
        || name.contains("preloader")
}
