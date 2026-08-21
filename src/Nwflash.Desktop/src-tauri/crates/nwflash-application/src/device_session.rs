use nwflash_domain::{
    parse_fastboot_rs_output, DeviceConnectionState, DeviceSnapshot, DomainError,
};
use nwflash_windows::{PlatformDeviceDiscovery, ProcessExecutor};

pub trait DeviceDiscovery: Send + Sync {
    fn discover_adb(&self) -> Result<String, DomainError>;
    fn discover_fastboot(&self) -> Result<String, DomainError>;
}

impl<E> DeviceDiscovery for PlatformDeviceDiscovery<E>
where
    E: ProcessExecutor,
{
    fn discover_adb(&self) -> Result<String, DomainError> {
        PlatformDeviceDiscovery::discover_adb(self)
    }

    fn discover_fastboot(&self) -> Result<String, DomainError> {
        PlatformDeviceDiscovery::discover_fastboot(self)
    }
}

pub struct DeviceSession;

impl DeviceSession {
    pub fn refresh(discovery: &dyn DeviceDiscovery) -> Result<DeviceSnapshot, DomainError> {
        let adb_snapshot = parse_fastboot_rs_output(&discovery.discover_adb()?);
        if adb_snapshot.connection_state != DeviceConnectionState::Disconnected {
            return Ok(adb_snapshot);
        }

        Ok(parse_fastboot_rs_output(&discovery.discover_fastboot()?))
    }
}
