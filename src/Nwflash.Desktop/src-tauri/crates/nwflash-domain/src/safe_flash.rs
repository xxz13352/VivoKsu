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
        || name.strip_prefix("lk").is_some_and(|rest| {
            rest.starts_with('_') || rest.starts_with(|c: char| c.is_ascii_digit())
        })
        || name.contains("preloader")
}

/// 受保护分区：留在刷写队列里，但**绝不真正写入设备**。
///
/// 两类分区受保护，与是否勾选“安全刷写”的对应关系是：
///
/// | 分区 | 未勾选安全刷写 | 勾选安全刷写 |
/// |---|---|---|
/// | `lk` / `lk_…` / `lk<数字>` / 含 `preloader` | 受保护 | 受保护 |
/// | system、product、vendor、odm、system_ext、odm_dlkm、system_dlkm、vendor_dlkm | 正常刷写 | 受保护 |
///
/// 受保护分区仍然出现在刷写队列与日志里（假装刷入），只是不派发真实
/// fastboot 命令；带槽位后缀的变体（`system_a`、`lk_b`）按基名判定。
pub fn should_simulate_safe_flash_partition(partition_name: &str, safe_flash: bool) -> bool {
    if should_skip_safe_flash_partition(partition_name) {
        return true;
    }
    let name = partition_name.to_ascii_lowercase();
    let base = name.strip_suffix("_a").or_else(|| name.strip_suffix("_b")).unwrap_or(&name);
    safe_flash && matches!(base,
        "system" | "product" | "vendor" | "odm" | "system_ext"
        | "odm_dlkm" | "system_dlkm" | "vendor_dlkm")
}

/// 「保留 ROOT」要保护、因此只做假刷写的启动分区。
///
/// 带槽位后缀的变体（`boot_a`、`init_boot_b`）必须按基名判定：勾选保留 ROOT
/// 时它们同样不能被覆盖，否则当前槽位的 boot 会被写掉。
pub fn should_simulate_keep_root_partition(partition_name: &str) -> bool {
    let name = partition_name.to_ascii_lowercase();
    let base = name.strip_suffix("_a").or_else(|| name.strip_suffix("_b")).unwrap_or(&name);
    matches!(base, "boot" | "init_boot" | "vendor_boot")
}

/// 「假刷写」的唯一判定入口：命中者**留在刷写队列里**、日志照常显示刷入、
/// 计数与进度也照常推进，但绝不派发 `fastboot flash`——只按镜像大小
/// ÷ 35MB/s 等出与真机一致的耗时。
///
/// 命中条件是两类分区的并集：
/// 1. 安全刷写规则下的受保护分区，见 [`should_simulate_safe_flash_partition`]；
/// 2. 勾选「保留 ROOT」时的启动分区，见 [`should_simulate_keep_root_partition`]。
pub fn should_simulate_partition_flash(
    partition_name: &str,
    safe_flash: bool,
    keep_root: bool,
) -> bool {
    should_simulate_safe_flash_partition(partition_name, safe_flash)
        || (keep_root && should_simulate_keep_root_partition(partition_name))
}
