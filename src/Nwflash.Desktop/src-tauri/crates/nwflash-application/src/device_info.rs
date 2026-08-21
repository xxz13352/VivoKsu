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
    details
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
    details.active_slot = normalize_slot(current_slot);
    details.bootloader_state = parse_bootloader_state(unlocked);
    details
}

pub fn parse_adb_battery_level(output: &str) -> String {
    output
        .lines()
        .map(str::trim)
        .find_map(|line| {
            line.strip_prefix("level:")
                .or_else(|| line.strip_prefix("LEVEL:"))
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
