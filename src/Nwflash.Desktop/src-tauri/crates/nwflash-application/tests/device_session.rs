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
fn refresh_prefers_an_authorized_adb_device_over_fastboot() {
    let discovery = RecordedDiscovery {
        adb_output: "List of devices attached\nADB-1\tdevice product:PD model:V2318A\n".to_string(),
        fastboot_output: "FAST-1\tfastboot\n".to_string(),
    };
    let snapshot = DeviceSession::refresh(&discovery).expect("refresh should parse ADB output");
    assert_eq!(
        snapshot.connection_state,
        DeviceConnectionState::AdbConnected
    );
    assert_eq!(snapshot.serial, "ADB-1");
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
