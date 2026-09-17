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
        // 对齐 C# `PlatformToolsNativeApi`：合并 ADB 与 Fastboot 两侧的设备列表，
        // 合计多于一台时冻结（多设备保护）。绝不能因“ADB 优先短路”隐藏并存的
        // fastboot 设备——多设备环境下把操作施加到错误目标属于变砖风险。
        let adb_snapshot = parse_fastboot_rs_output(&discovery.discover_adb()?);
        let adb_connected = adb_snapshot.connection_state != DeviceConnectionState::Disconnected;
        let fastboot_snapshot = parse_fastboot_rs_output(&discovery.discover_fastboot()?);
        let fastboot_connected =
            fastboot_snapshot.connection_state != DeviceConnectionState::Disconnected;

        match (adb_connected, fastboot_connected) {
            (true, false) => Ok(adb_snapshot),
            (false, false) | (false, true) => Ok(fastboot_snapshot),
            (true, true) => Err(DomainError::DeviceUnavailable(
                "检测到多台设备（ADB 与 Fastboot 设备并存），已暂停操作。请仅连接目标设备后重试。"
                    .to_string(),
            )),
        }
    }
}
