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

pub fn should_skip_safe_flash_partition(partition_name: &str) -> bool {
    partition_name.eq_ignore_ascii_case("lk") || partition_name.to_lowercase().contains("preloader")
}
