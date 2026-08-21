//! Minimal file transfer helpers over adb-root transport.

use crate::command_spec::CommandSpec;
use nwflash_domain::DomainError;
use nwflash_windows::device_transport::DeviceTransport;
use nwflash_windows::platform_tools::PlatformTools;

#[derive(Debug, Clone)]
pub struct FileTransferService {
    transport: DeviceTransport,
}

impl FileTransferService {
    pub const DEFAULT_ADB_EXECUTABLE: &'static str = "adb.exe";
    pub const DEFAULT_FASTBOOT_EXECUTABLE: &'static str = "fastboot.exe";

    pub fn new(transport: DeviceTransport) -> Self {
        Self { transport }
    }

    /// Uses the `adb.exe`/`fastboot.exe` shipped under `resources/platform-tools`
    /// so transfers work on machines without Android platform-tools on `PATH`.
    pub fn with_default_tools() -> Self {
        Self::new(DeviceTransport::new(PlatformTools::bundled()))
    }

    pub fn with_platform_tools(
        adb_executable: impl Into<String>,
        fastboot_executable: impl Into<String>,
    ) -> Self {
        let tools = PlatformTools::new(adb_executable, fastboot_executable);
        Self::new(DeviceTransport::new(tools))
    }

    pub fn build_pull_command(
        &self,
        serial: &str,
        device_path: &str,
        local_path: &str,
    ) -> Result<CommandSpec, DomainError> {
        self.transport
            .build_adb_root_copy_from_device_command(serial, device_path, local_path)
            .map(CommandSpec::from)
    }

    pub fn build_push_command(
        &self,
        serial: &str,
        local_path: &str,
        device_path: &str,
    ) -> Result<CommandSpec, DomainError> {
        self.transport
            .build_adb_root_copy_to_device_command(serial, local_path, device_path)
            .map(CommandSpec::from)
    }
}
