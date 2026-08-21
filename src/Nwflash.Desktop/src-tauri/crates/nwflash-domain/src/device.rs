use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum DeviceConnectionState {
    Disconnected,
    Unauthorized,
    MultipleDevices,
    AdbConnected,
    FastbootConnected,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceSnapshot {
    pub connection_state: DeviceConnectionState,
    pub serial: String,
    pub connection_label: String,
    pub model: String,
    pub android_version: String,
    pub battery_level: String,
}

impl DeviceSnapshot {
    pub fn disconnected() -> Self {
        Self {
            connection_state: DeviceConnectionState::Disconnected,
            serial: "--".to_string(),
            connection_label: "等待连接".to_string(),
            model: "未检测到设备".to_string(),
            android_version: "--".to_string(),
            battery_level: "--".to_string(),
        }
    }

    pub fn multiple_devices() -> Self {
        Self {
            connection_state: DeviceConnectionState::MultipleDevices,
            serial: "--".to_string(),
            connection_label: "检测到多台设备".to_string(),
            model: "未检测到设备".to_string(),
            android_version: "--".to_string(),
            battery_level: "--".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceDetailsSnapshot {
    pub brand: String,
    pub model: String,
    pub codename: String,
    pub serial: String,
    pub android_version: String,
    pub firmware_version: String,
    pub kernel_version: String,
    pub active_slot: String,
    pub bootloader_state: String,
    pub verified_boot_state: String,
    pub usb_debugging_state: String,
    pub build_time: String,
}

impl DeviceDetailsSnapshot {
    pub fn empty() -> Self {
        Self {
            brand: "--".to_string(),
            model: "未检测到设备".to_string(),
            codename: "--".to_string(),
            serial: "--".to_string(),
            android_version: "--".to_string(),
            firmware_version: "--".to_string(),
            kernel_version: "--".to_string(),
            active_slot: "--".to_string(),
            bootloader_state: "--".to_string(),
            verified_boot_state: "--".to_string(),
            usb_debugging_state: "--".to_string(),
            build_time: "--".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceFileEntry {
    pub name: String,
    pub full_path: String,
    pub is_directory: bool,
    pub size_bytes: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum DeviceRefreshMode {
    Manual,
    Automatic,
}

/// Splits one `adb devices [-l]` / `fastboot devices [-l]` row into
/// `(serial, state_text)`.
///
/// Plain `adb devices` separates the two columns with a single TAB, but
/// `adb devices -l` (what `PlatformTools::adb_devices_command` actually runs)
/// pads with spaces and appends `key:value` columns, so splitting on TAB alone
/// drops every row.  Mirrors `PlatformToolsNativeApi.ParseAdbDevices`, which
/// splits on both TAB and space.  The remainder of the line is kept intact
/// because some states are multi-word (`no permissions (user in plugdev group)`).
fn split_device_line(line: &str) -> Option<(&str, &str)> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    // Both `adb devices` and `adb devices -l` print this banner first.
    if line.len() >= 15 && line[..15].eq_ignore_ascii_case("list of devices") {
        return None;
    }
    // `* daemon not running; starting now at tcp:5037` and friends.
    if line.starts_with('*') {
        return None;
    }

    let (serial, rest) = line.split_once(char::is_whitespace)?;
    let rest = rest.trim();
    if serial.is_empty() || rest.is_empty() {
        return None;
    }
    Some((serial, rest))
}

pub fn parse_fastboot_rs_output(output: &str) -> DeviceSnapshot {
    let devices: Vec<(String, String)> = output
        .lines()
        .filter_map(split_device_line)
        .map(|(serial, state)| (serial.to_string(), state.to_string()))
        .collect();

    if devices.is_empty() {
        return DeviceSnapshot::disconnected();
    }

    if devices.len() > 1 {
        return DeviceSnapshot::multiple_devices();
    }

    let (serial, raw_mode) = &devices[0];
    let mode = raw_mode.to_lowercase();

    if mode.contains("unauthorized") {
        return DeviceSnapshot {
            connection_state: DeviceConnectionState::Unauthorized,
            serial: serial.to_string(),
            connection_label: "等待 USB 调试授权".to_string(),
            model: "未检测到设备".to_string(),
            android_version: "--".to_string(),
            battery_level: "--".to_string(),
        };
    }

    if mode.contains("fastbootd") {
        return DeviceSnapshot {
            connection_state: DeviceConnectionState::FastbootConnected,
            serial: serial.to_string(),
            connection_label: "Fastbootd 已连接".to_string(),
            model: "未检测到设备".to_string(),
            android_version: "--".to_string(),
            battery_level: "--".to_string(),
        };
    }

    if mode.contains("fastboot") {
        return DeviceSnapshot {
            connection_state: DeviceConnectionState::FastbootConnected,
            serial: serial.to_string(),
            connection_label: "Fastboot 已连接".to_string(),
            model: "未检测到设备".to_string(),
            android_version: "--".to_string(),
            battery_level: "--".to_string(),
        };
    }

    if mode.contains("offline") {
        return DeviceSnapshot {
            connection_state: DeviceConnectionState::Disconnected,
            serial: serial.to_string(),
            connection_label: "设备离线".to_string(),
            model: "未检测到设备".to_string(),
            android_version: "--".to_string(),
            battery_level: "--".to_string(),
        };
    }

    if mode.contains("no permissions") {
        return DeviceSnapshot {
            connection_state: DeviceConnectionState::Unauthorized,
            serial: serial.to_string(),
            connection_label: "无 USB 调试权限".to_string(),
            model: "未检测到设备".to_string(),
            android_version: "--".to_string(),
            battery_level: "--".to_string(),
        };
    }

    if mode.starts_with("device") {
        return DeviceSnapshot {
            connection_state: DeviceConnectionState::AdbConnected,
            serial: serial.to_string(),
            connection_label: "ADB 已连接".to_string(),
            model: "未检测到设备".to_string(),
            android_version: "--".to_string(),
            battery_level: "--".to_string(),
        };
    }

    DeviceSnapshot {
        connection_state: DeviceConnectionState::Error,
        serial: serial.to_string(),
        connection_label: "未知设备状态".to_string(),
        model: "未检测到设备".to_string(),
        android_version: "--".to_string(),
        battery_level: "--".to_string(),
    }
}
