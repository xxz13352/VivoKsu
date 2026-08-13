use std::io;
use std::path::Path;

pub struct AdbClient;

impl AdbClient {
    pub fn enumerate_adb_devices() -> io::Result<Vec<AdbDeviceInfo>> {
        Ok(Vec::new())
    }

    pub fn connect_fast(_serial: Option<&str>, _verbose: bool) -> io::Result<Self> {
        Ok(AdbClient)
    }

    pub fn connect_with_auth(_serial: Option<&str>) -> io::Result<Self> {
        Ok(AdbClient)
    }

    pub fn serial(&self) -> &str {
        "unknown"
    }

    pub fn reboot(&mut self, _mode: Option<&str>) -> io::Result<()> {
        Ok(())
    }

    pub fn shell(&mut self, _command: &str) -> io::Result<String> {
        Ok(String::new())
    }

    pub fn push(&mut self, _local: &Path, _remote: &Path) -> io::Result<()> {
        Ok(())
    }

    pub fn pull(&mut self, _remote: &Path, _local: &Path) -> io::Result<u64> {
        Ok(0)
    }

    pub fn stat(&mut self, _remote: &Path) -> io::Result<FileInfo> {
        Ok(FileInfo::default())
    }

    pub fn list(&mut self, _remote: &Path) -> io::Result<Vec<(String, FileInfo)>> {
        Ok(Vec::new())
    }

    pub fn get_prop(&mut self, _prop: &str) -> io::Result<String> {
        Ok(String::new())
    }
}

#[derive(Debug, Clone, Default)]
pub struct AdbDeviceInfo {
    pub serial: String,
    pub state: String,
    pub product: Option<String>,
    pub model: Option<String>,
    pub device: Option<String>,
    pub transport_id: Option<u32>,
}

#[derive(Debug, Clone, Default)]
pub struct FileInfo {
    pub mode: u32,
    pub size: u64,
    pub mtime: u64,
}

pub fn adb_cli_shell_proxy(
    _serial: &Option<String>,
    _command: &[String],
    _verbose: bool,
) -> io::Result<()> {
    Ok(())
}

pub fn adb_cli_reboot_proxy(
    _serial: &Option<String>,
    _target: Option<&str>,
    _verbose: bool,
) -> io::Result<()> {
    Ok(())
}

pub fn adb_cli_push_proxy(
    _serial: &Option<String>,
    _local: &Path,
    _remote: &Path,
    _verbose: bool,
) -> io::Result<()> {
    Ok(())
}

pub fn adb_cli_pull_proxy(
    _serial: &Option<String>,
    _remote: &Path,
    _local: &Path,
    _verbose: bool,
) -> io::Result<()> {
    Ok(())
}

pub fn adb_cli_install_proxy(
    _serial: &Option<String>,
    _apk: &Path,
    _verbose: bool,
) -> io::Result<()> {
    Ok(())
}

pub fn adb_cli_uninstall_proxy(
    _serial: &Option<String>,
    _package: &str,
    _verbose: bool,
) -> io::Result<()> {
    Ok(())
}

pub fn adb_cli_packages_proxy(
    _serial: &Option<String>,
    _verbose: bool,
) -> io::Result<Vec<String>> {
    Ok(Vec::new())
}

pub fn adb_cli_logcat_proxy(
    _serial: &Option<String>,
    _verbose: bool,
) -> io::Result<()> {
    Ok(())
}

pub fn adb_cli_screencap_proxy(
    _serial: &Option<String>,
    _output: &Path,
    _verbose: bool,
) -> io::Result<()> {
    Ok(())
}

pub fn adb_cli_screenrecord_proxy(
    _serial: &Option<String>,
    _output: &Path,
    _time_limit: u32,
    _verbose: bool,
) -> io::Result<()> {
    Ok(())
}
