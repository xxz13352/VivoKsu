use std::collections::HashMap;

use nwflash_domain::DeviceDetailsSnapshot;

pub fn parse_adb_device_details(serial: &str, output: &str) -> DeviceDetailsSnapshot {
    let properties = parse_getprop(output);
    let mut details = DeviceDetailsSnapshot::empty();
    details.brand = property(&properties, "ro.product.brand");
    details.model = property(&properties, "ro.product.model");
    details.codename = property(&properties, "ro.product.device");
    details.serial = serial.to_string();
    details.android_version = property(&properties, "ro.build.version.release");
    details.firmware_version = property(&properties, "ro.build.display.id");
    // 槽位 / 引导加载器锁定态 / 验证启动状态都在同一次 `getprop` 全量输出里，
    // 不额外起进程（C# DeviceInfoService.ReadAdbAsync 用单独 shell 调用读同一批属性）。
    details.active_slot = normalize_slot(&property(&properties, "ro.boot.slot_suffix"));
    details.bootloader_state =
        parse_bootloader_state(&property(&properties, "ro.boot.flash.locked"));
    details.verified_boot_state = property(&properties, "ro.boot.verifiedbootstate");
    details
}

/// `adb shell uname -r` 输出 → 内核版本。多行输出取第一行非空内容，
/// 空输出降级为 `Not available`，由上层投影成界面占位符。
pub fn parse_kernel_version(output: &str) -> String {
    let value = output
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    value_or_unavailable(value)
}

/// C# `DeviceSessionService.IsSameDevice`：已知档案与当前序列号属于同一台设备。
pub fn is_same_device(details: &DeviceDetailsSnapshot, serial: &str) -> bool {
    let serial = serial.trim();
    !serial.is_empty() && serial != "--" && details.serial == serial
}

/// Fastboot 分支的档案起点。对应 C# `DeviceSessionService.RefreshAsync`：
///
/// ```csharp
/// details = IsSameDevice(knownDetails, snapshot.Serial)
///     ? knownDetails with { Serial = snapshot.Serial }
///     : DeviceDetailsSnapshot.Empty with { Model = snapshot.Model, Serial = snapshot.Serial };
/// ```
///
/// 同一台设备沿用上一次（通常是 ADB 侧）读到的档案——设备重启进 fastboot 不会
/// 让系统版本/内核/验证启动失效，丢掉它们只会让概览在 fastboot 下只剩型号。
/// 换设备则从空档案起步，绝不把上一台设备的信息带过去。
pub fn fastboot_details_seed(known: &DeviceDetailsSnapshot, serial: &str) -> DeviceDetailsSnapshot {
    let serial = serial.trim().to_string();
    if is_same_device(known, &serial) {
        let mut seed = known.clone();
        seed.serial = serial;
        return seed;
    }

    let mut empty = DeviceDetailsSnapshot::empty();
    empty.serial = serial;
    empty
}

pub fn apply_fastboot_device_details(
    mut details: DeviceDetailsSnapshot,
    current_slot: &str,
    unlocked: &str,
    product: &str,
) -> DeviceDetailsSnapshot {
    let product = value_or_unavailable(product);
    if is_unavailable(&details.model) {
        details.model = product.clone();
    }
    if is_unavailable(&details.codename) {
        details.codename = product;
    }
    // 只在 getvar 真的读到时覆盖：变量失败（老机型不支持 `current-slot` /
    // `unlocked`）不该把已知档案擦成「未读取」。
    let slot = normalize_slot(current_slot);
    if !is_unavailable(&slot) {
        details.active_slot = slot;
    }
    let bootloader_state = parse_bootloader_state(unlocked);
    if !is_unavailable(&bootloader_state) {
        details.bootloader_state = bootloader_state;
    }
    details
}

pub fn parse_adb_battery_level(output: &str) -> String {
    output
        .lines()
        .map(str::trim)
        // dumpsys 实际只输出小写 "level:"，但保持与 C# 参考一致的
        // 大小写不敏感前缀匹配，避免异常机型输出 "Level:" 时漏读。
        .find_map(|line| {
            let (first, rest) = line.split_at(line.len().min("level:".len()));
            first.eq_ignore_ascii_case("level:").then_some(rest)
        })
        .and_then(|value| value.trim().parse::<u8>().ok())
        .filter(|value| *value <= 100)
        .map(|value| format!("{value}%"))
        .unwrap_or_else(|| "--".to_string())
}

fn parse_getprop(output: &str) -> HashMap<&str, &str> {
    output
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let (key, value) = line.strip_prefix('[')?.split_once("]: [")?;
            Some((key, value.strip_suffix(']')?))
        })
        .collect()
}

fn property(properties: &HashMap<&str, &str>, key: &str) -> String {
    properties
        .get(key)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .unwrap_or("Not available")
        .to_string()
}

fn value_or_unavailable(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        "Not available".to_string()
    } else {
        value.to_string()
    }
}

fn normalize_slot(value: &str) -> String {
    value_or_unavailable(value.trim().trim_start_matches('_'))
}

fn parse_bootloader_state(value: &str) -> String {
    match value.trim().to_ascii_lowercase().as_str() {
        "0" | "yes" | "true" => "unlocked".to_string(),
        "1" | "no" | "false" => "locked".to_string(),
        _ => "Not available".to_string(),
    }
}

fn is_unavailable(value: &str) -> bool {
    value.trim().is_empty() || matches!(value, "--" | "Not available" | "未检测到设备")
}
