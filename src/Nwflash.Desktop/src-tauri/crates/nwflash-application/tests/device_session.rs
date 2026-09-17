use nwflash_application::{DeviceDiscovery, DeviceSession};
use nwflash_domain::{DeviceConnectionState, DeviceSnapshot, DomainError};

struct RecordedDiscovery {
    adb_output: String,
    fastboot_output: String,
}

impl DeviceDiscovery for RecordedDiscovery {
    fn discover_adb(&self) -> Result<String, DomainError> {
        Ok(self.adb_output.clone())
    }
    fn discover_fastboot(&self) -> Result<String, DomainError> {
        Ok(self.fastboot_output.clone())
    }
}

#[test]
fn refresh_freezes_when_adb_and_fastboot_devices_coexist() {
    // N11 回归（对齐 C# 多设备冻结）：ADB 设备与 Fastboot 设备并存时，
    // ADB 优先短路会隐藏 fastboot 侧的设备，把操作施加到错误目标属于
    // 变砖风险——必须冻结并要求只连接目标设备。
    let discovery = RecordedDiscovery {
        adb_output: "List of devices attached\nADB-1\tdevice product:PD model:V2318A\n".to_string(),
        fastboot_output: "FAST-1\tfastboot\n".to_string(),
    };
    let error =
        DeviceSession::refresh(&discovery).expect_err("coexisting devices must freeze refresh");
    assert!(error.to_string().contains("多台设备"));
}

#[test]
fn refresh_falls_back_to_fastboot_when_adb_has_no_device() {
    let discovery = RecordedDiscovery {
        adb_output: "List of devices attached\n\n".to_string(),
        fastboot_output: "FAST-1\tfastboot\n".to_string(),
    };
    let snapshot =
        DeviceSession::refresh(&discovery).expect("refresh should parse fastboot output");
    assert_eq!(
        snapshot.connection_state,
        DeviceConnectionState::FastbootConnected
    );
    assert_eq!(snapshot.serial, "FAST-1");
}

#[test]
fn refresh_returns_disconnected_when_no_transport_reports_a_device() {
    let discovery = RecordedDiscovery {
        adb_output: "List of devices attached\n\n".to_string(),
        fastboot_output: String::new(),
    };
    assert_eq!(
        DeviceSession::refresh(&discovery).expect("empty discovery should be valid"),
        DeviceSnapshot::disconnected()
    );
}
