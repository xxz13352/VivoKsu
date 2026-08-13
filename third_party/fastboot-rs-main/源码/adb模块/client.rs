
use std::io;
use std::path::Path;
use std::time::Duration;

use crate::adb::connection::{AdbConnection, AdbTransport};
use crate::adb::shell::ShellSession;
use crate::adb::sync::{self, SyncSession, FileInfo};
use crate::adb::protocol::DeviceState;
use crate::error::TransportError;
use crate::usb_transport::{UsbTransport, UsbDeviceInfo};

pub struct UsbAdbTransport {
    inner: UsbTransport,
}

impl UsbAdbTransport {
    pub fn new(transport: UsbTransport) -> Self {
        Self { inner: transport }
    }
}

impl AdbTransport for UsbAdbTransport {
    fn write_all(&mut self, data: &[u8]) -> Result<(), TransportError> {
        let mut written = 0;
        while written < data.len() {
            let n = self.inner.bulk_write_sync(&data[written..])?;
            if n == 0 {
                return Err(TransportError::Usb("写入失败".into()));
            }
            written += n;
        }
        Ok(())
    }

    fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), TransportError> {
        let mut read = 0;
        while read < buf.len() {
            let n = self.inner.bulk_read_sync(&mut buf[read..])?;
            if n == 0 {
                return Err(TransportError::Usb("读取失败".into()));
            }
            read += n;
        }
        Ok(())
    }

    fn set_timeout(&mut self, timeout: Duration) {
        self.inner.set_timeout(timeout);
    }
}

#[derive(Debug, Clone)]
pub struct AdbDeviceInfo {
    pub serial: String,
    pub state: DeviceState,
    pub product: Option<String>,
    pub model: Option<String>,
    pub device: Option<String>,
    pub transport_id: Option<u32>,
}

pub struct AdbClient {
    conn: AdbConnection<UsbAdbTransport>,
    serial: String,
}

impl AdbClient {
    pub fn enumerate_adb_devices() -> io::Result<Vec<AdbDeviceInfo>> {
        let usb_devices = UsbTransport::enumerate_adb_devices()
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

        let mut adb_devices = Vec::new();
        for dev in usb_devices {
            let real_serial = Self::get_real_serial_for_device(&dev)
                .unwrap_or_else(|| dev.serial_number.clone());

            adb_devices.push(AdbDeviceInfo {
                serial: real_serial,
                state: DeviceState::Device,
                product: dev.product_name.clone(),
                model: None,
                device: None,
                transport_id: None,
            });
        }
        Ok(adb_devices)
    }

    fn get_real_serial_for_device(dev: &UsbDeviceInfo) -> Option<String> {
        let is_fake_serial = dev.serial_number.contains('&')
            || (dev.serial_number.contains(':') && dev.serial_number.len() == 9);

        if !is_fake_serial {
            return Some(dev.serial_number.clone());
        }

        let transport = match UsbTransport::open_adb_by_info(dev) {
            Ok(t) => t,
            Err(_) => return None,
        };

        let adb_transport = UsbAdbTransport::new(transport);
        let mut conn = AdbConnection::new_fast(adb_transport, false);

        if conn.connect().is_err() {
            return None;
        }

        match ShellSession::execute(&mut conn, "getprop ro.serialno") {
            Ok(output) => {
                let serial = output.trim().to_string();
                if !serial.is_empty() && serial.len() < 64 && !serial.contains('&') {
                    return Some(serial);
                }
            }
            Err(_) => {}
        }

        None
    }

    pub fn list_devices() -> io::Result<Vec<AdbDeviceInfo>> {
        Self::enumerate_adb_devices()
    }

    pub fn connect(serial: Option<&str>) -> io::Result<Self> {
        Self::connect_with_auth(serial)
    }

    pub fn connect_fast(serial: Option<&str>, verbose: bool) -> io::Result<Self> {
        let transport = UsbTransport::open_adb(serial)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

        let serial_str = serial.unwrap_or("unknown").to_string();
        let adb_transport = UsbAdbTransport::new(transport);
        let mut conn = AdbConnection::new_fast(adb_transport, verbose);

        conn.connect().map_err(|e| {
            let msg = format!(
                "{}\n\n可能原因:\n\
                 1. 设备需要授权 - 请检查设备屏幕\n\
                 2. USB 调试未启用\n\
                 3. 设备被其他程序占用",
                e
            );
            io::Error::new(e.kind(), msg)
        })?;

        Ok(Self {
            conn,
            serial: serial_str,
        })
    }

    pub fn connect_with_auth(serial: Option<&str>) -> io::Result<Self> {
        let transport = UsbTransport::open_adb(serial)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

        let serial_str = serial.unwrap_or("unknown").to_string();
        let adb_transport = UsbAdbTransport::new(transport);
        let mut conn = AdbConnection::new_with_auth_wait(adb_transport);

        conn.connect().map_err(|e| {
            let msg = format!(
                "{}\n\n可能原因:\n\
                 1. 设备需要授权 - 请检查设备屏幕\n\
                 2. USB 调试未启用\n\
                 3. 设备被其他程序占用",
                e
            );
            io::Error::new(e.kind(), msg)
        })?;

        Ok(Self {
            conn,
            serial: serial_str,
        })
    }

    pub fn serial(&self) -> &str {
        &self.serial
    }

    pub fn shell(&mut self, command: &str) -> io::Result<String> {
        ShellSession::execute(&mut self.conn, command)
    }

    pub fn interactive_shell(&mut self) -> io::Result<()> {
        crate::adb::shell::run_interactive_shell(&mut self.conn)
    }

    pub fn open_shell(&mut self, command: Option<&str>) -> io::Result<u32> {
        let destination = match command {
            Some(cmd) => format!("shell:{}", cmd),
            None => "shell:".to_string(),
        };
        self.conn.open_stream(&destination)
    }

    pub fn read_shell_output(&mut self, stream_id: u32) -> io::Result<Option<Vec<u8>>> {
        self.conn.read_stream(stream_id)
    }

    pub fn write_shell_input(&mut self, stream_id: u32, data: &[u8]) -> io::Result<()> {
        self.conn.write_stream(stream_id, data)
    }

    pub fn close_shell(&mut self, stream_id: u32) -> io::Result<()> {
        self.conn.close_stream(stream_id)
    }

    pub fn push(&mut self, local: &Path, remote: &str) -> io::Result<()> {
        sync::push_file(&mut self.conn, local, remote)
    }

    pub fn pull(&mut self, remote: &str, local: &Path) -> io::Result<u64> {
        sync::pull_file(&mut self.conn, remote, local)
    }

    pub fn stat(&mut self, path: &str) -> io::Result<FileInfo> {
        let mut session = SyncSession::open(&mut self.conn)?;
        let info = session.stat(path)?;
        session.close()?;
        Ok(info)
    }

    pub fn reboot(&mut self, mode: Option<&str>) -> io::Result<()> {
        let cmd = match mode {
            Some("bootloader") => "reboot:bootloader",
            Some("recovery") => "reboot:recovery",
            Some("sideload") => "reboot:sideload",
            Some("sideload-auto-reboot") => "reboot:sideload-auto-reboot",
            Some("fastboot") => "reboot:fastboot",
            Some(m) => return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("未知重启模式: {}", m),
            )),
            None => "reboot:",
        };

        match self.conn.open_stream(cmd) {
            Ok(stream_id) => {
                let _ = self.conn.close_stream(stream_id);
            }
            Err(_) => {
            }
        }
        Ok(())
    }

    pub fn get_prop(&mut self, name: &str) -> io::Result<String> {
        let output = self.shell(&format!("getprop {}", name))?;
        Ok(output.trim().to_string())
    }

    pub fn ls(&mut self, path: &str) -> io::Result<String> {
        self.shell(&format!("ls -la {}", path))
    }

    pub fn is_root(&mut self) -> io::Result<bool> {
        let output = self.shell("id")?;
        Ok(output.contains("uid=0"))
    }

    pub fn get_device_info(&mut self) -> io::Result<DeviceInfo> {
        Ok(DeviceInfo {
            serial: self.serial.clone(),
            product: self.get_prop("ro.product.name").ok(),
            model: self.get_prop("ro.product.model").ok(),
            device: self.get_prop("ro.product.device").ok(),
            sdk_version: self.get_prop("ro.build.version.sdk").ok()
                .and_then(|s| s.parse().ok()),
            android_version: self.get_prop("ro.build.version.release").ok(),
        })
    }
}

#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub serial: String,
    pub product: Option<String>,
    pub model: Option<String>,
    pub device: Option<String>,
    pub sdk_version: Option<u32>,
    pub android_version: Option<String>,
}

fn is_adb_device(dev: &UsbDeviceInfo) -> bool {
    const FASTBOOT_DEVICES: &[(u16, u16)] = &[
        (0x18d1, 0xd00d),
        (0x18d1, 0x4ee0),
        (0x05c6, 0x9008),
        (0x05c6, 0x9006),
        (0x05c6, 0x9025),
        (0x05c6, 0x900e),
        (0x05c6, 0x90db),
        (0x2717, 0xff40),
        (0x2717, 0xff48),
        (0x22b8, 0x2281),
        (0x0bb4, 0x0fff),
        (0x04e8, 0x6601),
        (0x12d1, 0x1057),
        (0x19d2, 0x0016),
        (0x2a70, 0x9011),
        (0x0e8d, 0x201c),
        (0x0e8d, 0x0003),
    ];

    if FASTBOOT_DEVICES.contains(&(dev.vendor_id, dev.product_id)) {
        return false;
    }

    const GENERIC_FASTBOOT_PIDS: &[u16] = &[
        0xd00d,
        0x0fff,
    ];

    if GENERIC_FASTBOOT_PIDS.contains(&dev.product_id) {
        return false;
    }

    const ADB_VENDOR_IDS: &[u16] = &[
        0x18d1,
        0x0bb4,
        0x04e8,
        0x22b8,
        0x1004,
        0x12d1,
        0x0fce,
        0x19d2,
        0x2717,
        0x1782,
        0x0e8d,
        0x2a70,
        0x1949,
        0x2ae5,
        0x0502,
        0x0b05,
        0x413c,
        0x0489,
        0x091e,
        0x109b,
        0x2116,
        0x0482,
        0x17ef,
        0x1ebf,
        0x2080,
        0x10a9,
        0x1d4d,
        0x04da,
        0x05c6,
        0x1f3a,
        0x2207,
        0x2836,
        0x2a45,
        0x0e79,
        0x1bbb,
        0x0409,
        0x2b4c,
        0x2d95,
    ];

    if ADB_VENDOR_IDS.contains(&dev.vendor_id) {
        return true;
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_list_devices() {
        let _ = AdbClient::list_devices();
    }
}