use nwflash_application::{DeviceMonitor, MonitorRefreshResult};
use nwflash_domain::{DeviceConnectionState, DeviceRefreshMode, DeviceSnapshot};

fn adb(serial: &str) -> DeviceSnapshot {
    DeviceSnapshot {
        connection_state: DeviceConnectionState::AdbConnected,
        serial: serial.to_string(),
        connection_label: "ADB 已连接".to_string(),
        model: "--".to_string(),
        android_version: "--".to_string(),
        battery_level: "--".to_string(),
    }
}

#[test]
fn heartbeat_broadcasts_only_after_the_device_identity_changes() {
    let mut monitor = DeviceMonitor::new(DeviceSnapshot::disconnected());

    assert_eq!(
        monitor.refresh(adb("SN-1"), false, DeviceRefreshMode::Automatic),
        MonitorRefreshResult::AppliedAndBroadcast
    );
    assert_eq!(
        monitor.refresh(adb("SN-1"), false, DeviceRefreshMode::Automatic),
        MonitorRefreshResult::Applied
    );
    assert_eq!(
        monitor.refresh(adb("SN-2"), false, DeviceRefreshMode::Automatic),
        MonitorRefreshResult::AppliedAndBroadcast
    );
}

#[test]
fn automatic_disconnect_requires_two_consecutive_empty_discoveries() {
    let mut monitor = DeviceMonitor::new(adb("SN-1"));

    assert_eq!(
        monitor.refresh(
            DeviceSnapshot::disconnected(),
            false,
            DeviceRefreshMode::Automatic
        ),
        MonitorRefreshResult::Deferred
    );
    assert_eq!(monitor.snapshot(), &adb("SN-1"));
    assert_eq!(
        monitor.refresh(
            DeviceSnapshot::disconnected(),
            false,
            DeviceRefreshMode::Automatic
        ),
        MonitorRefreshResult::AppliedAndBroadcast
    );
    assert_eq!(monitor.snapshot(), &DeviceSnapshot::disconnected());
}

#[test]
fn manual_refresh_broadcasts_even_when_identity_is_unchanged() {
    let mut monitor = DeviceMonitor::new(adb("SN-1"));

    assert_eq!(
        monitor.refresh(adb("SN-1"), false, DeviceRefreshMode::Manual),
        MonitorRefreshResult::AppliedAndBroadcast
    );
}

#[test]
fn automatic_refresh_is_skipped_while_a_device_operation_is_running() {
    let mut monitor = DeviceMonitor::new(adb("SN-1"));

    assert_eq!(
        monitor.refresh(adb("SN-2"), true, DeviceRefreshMode::Automatic),
        MonitorRefreshResult::SkippedBusy
    );
    assert_eq!(monitor.snapshot(), &adb("SN-1"));
}

#[test]
fn automatic_errors_require_three_consecutive_discoveries_before_replacing_a_connected_snapshot() {
    let mut monitor = DeviceMonitor::new(adb("SN-1"));
    let error = DeviceSnapshot {
        connection_state: DeviceConnectionState::Error,
        serial: "--".to_string(),
        connection_label: "设备检测失败".to_string(),
        model: "未检测到设备".to_string(),
        android_version: "--".to_string(),
        battery_level: "--".to_string(),
    };

    assert_eq!(
        monitor.refresh(error.clone(), false, DeviceRefreshMode::Automatic),
        MonitorRefreshResult::Deferred
    );
    assert_eq!(monitor.snapshot(), &adb("SN-1"));
    assert_eq!(
        monitor.refresh(error.clone(), false, DeviceRefreshMode::Automatic),
        MonitorRefreshResult::Deferred
    );
    assert_eq!(monitor.snapshot(), &adb("SN-1"));
    assert_eq!(
        monitor.refresh(error.clone(), false, DeviceRefreshMode::Automatic),
        MonitorRefreshResult::AppliedAndBroadcast
    );
    assert_eq!(monitor.snapshot(), &error);
}
