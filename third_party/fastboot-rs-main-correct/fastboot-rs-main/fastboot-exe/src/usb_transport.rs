use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use crate::error::TransportError;
use crate::transport::{AsyncTransport, TransportStats};
const FASTBOOT_CLASS: u8 = 0xFF;
const FASTBOOT_SUBCLASS: u8 = 0x42;
const FASTBOOT_PROTOCOL: u8 = 0x03;
const ADB_PROTOCOL: u8 = 0x01;
const USB2_MAX_PACKET_SIZE: usize = 512;
const USB3_MAX_PACKET_SIZE: usize = 1024;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsbMode {
    Fastboot,
    Adb,
}

#[derive(Debug, Clone)]
pub struct UsbDeviceInfo {
    pub vendor_id: u16,
    pub product_id: u16,
    pub serial_number: String,
    pub device_path: String,
    pub is_usb3: bool,
    pub product_name: Option<String>,
    pub manufacturer: Option<String>,
}

#[cfg(target_os = "windows")]
pub use self::windows_impl::*;

#[cfg(target_os = "windows")]
mod windows_impl {
    use super::*;
    use std::ffi::OsStr;
    use std::ffi::OsString;
    use std::io::Write;
    use std::mem;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::ptr;
    use winapi::shared::guiddef::GUID;
    use winapi::shared::minwindef::*;
    use winapi::shared::winerror::*;
    use winapi::um::errhandlingapi::GetLastError;
    use winapi::um::fileapi::*;
    use winapi::um::handleapi::*;
    use winapi::um::ioapiset::*;
    use winapi::um::minwinbase::OVERLAPPED;
    use winapi::um::setupapi::*;
    use winapi::um::synchapi::*;
    use winapi::um::winbase::*;
    use winapi::um::winnt::*;
    const ADB_DEVICE_GUID: GUID = GUID {
        Data1: 0xF72FE0D4,
        Data2: 0xCBCB,
        Data3: 0x407D,
        Data4: [0x88, 0x14, 0x9E, 0xD6, 0x73, 0xD0, 0xDD, 0x6B],
    };
    const FILE_DEVICE_UNKNOWN: DWORD = 0x00000022;
    const fn ctl_code(device_type: DWORD, function: DWORD, method: DWORD, access: DWORD) -> DWORD {
        (device_type << 16) | (access << 14) | (function << 2) | method
    }

    const METHOD_BUFFERED: DWORD = 0;
    const METHOD_OUT_DIRECT: DWORD = 2;
    const FILE_READ_ACCESS: DWORD = 1;
    const FILE_WRITE_ACCESS: DWORD = 2;
    const ADB_CTL_GET_USB_DEVICE_DESCRIPTOR: DWORD = 10;
    #[allow(dead_code)]
    const ADB_CTL_GET_USB_CONFIGURATION_DESCRIPTOR: DWORD = 11;
    #[allow(dead_code)]
    const ADB_CTL_GET_USB_INTERFACE_DESCRIPTOR: DWORD = 12;
    #[allow(dead_code)]
    const ADB_CTL_GET_ENDPOINT_INFORMATION: DWORD = 13;
    #[allow(dead_code)]
    const ADB_CTL_BULK_READ: DWORD = 14;
    #[allow(dead_code)]
    const ADB_CTL_BULK_WRITE: DWORD = 15;
    const ADB_CTL_GET_SERIAL_NUMBER: DWORD = 16;

    const ADB_IOCTL_GET_USB_DEVICE_DESCRIPTOR: DWORD = ctl_code(
        FILE_DEVICE_UNKNOWN,
        ADB_CTL_GET_USB_DEVICE_DESCRIPTOR,
        METHOD_BUFFERED,
        FILE_READ_ACCESS,
    );
    const ADB_IOCTL_GET_SERIAL_NUMBER: DWORD = ctl_code(
        FILE_DEVICE_UNKNOWN,
        ADB_CTL_GET_SERIAL_NUMBER,
        METHOD_BUFFERED,
        FILE_READ_ACCESS,
    );
    #[allow(dead_code)]
    const ADB_IOCTL_BULK_READ: DWORD = ctl_code(
        FILE_DEVICE_UNKNOWN,
        ADB_CTL_BULK_READ,
        METHOD_OUT_DIRECT,
        FILE_READ_ACCESS,
    );
    #[allow(dead_code)]
    const ADB_IOCTL_BULK_WRITE: DWORD = ctl_code(
        FILE_DEVICE_UNKNOWN,
        ADB_CTL_BULK_WRITE,
        METHOD_BUFFERED,
        FILE_WRITE_ACCESS,
    );

    use winapi::shared::winusbio::WINUSB_PIPE_INFORMATION;
    use winapi::um::winusb::{
        WinUsb_AbortPipe, WinUsb_FlushPipe, WinUsb_Free, WinUsb_GetOverlappedResult,
        WinUsb_Initialize, WinUsb_QueryInterfaceSettings, WinUsb_QueryPipe, WinUsb_ReadPipe,
        WinUsb_ResetPipe, WinUsb_SetPipePolicy, WinUsb_WritePipe, USB_INTERFACE_DESCRIPTOR,
        WINUSB_INTERFACE_HANDLE,
    };
    #[allow(dead_code)]
    const USB_ENDPOINT_DIRECTION_MASK: u8 = 0x80;
    const USB_ENDPOINT_DIRECTION_IN: u8 = 0x80;
    #[allow(dead_code)]
    const USB_ENDPOINT_DIRECTION_OUT: u8 = 0x00;
    use winapi::shared::winusbio::PIPE_TRANSFER_TIMEOUT;
    #[allow(dead_code)]
    use winapi::shared::winusbio::RAW_IO;

    #[repr(C, packed)]
    #[derive(Debug, Clone, Copy, Default)]
    struct UsbDeviceDescriptor {
        b_length: u8,
        b_descriptor_type: u8,
        bcd_usb: u16,
        b_device_class: u8,
        b_device_sub_class: u8,
        b_device_protocol: u8,
        b_max_packet_size0: u8,
        id_vendor: u16,
        id_product: u16,
        bcd_device: u16,
        i_manufacturer: u8,
        i_product: u8,
        i_serial_number: u8,
        b_num_configurations: u8,
    }
    #[repr(C)]
    #[allow(dead_code)]
    struct AdbBulkTransfer {
        time_out: DWORD,
        transfer_size: DWORD,
        write_buffer: u64,
    }
    #[derive(Debug, Clone, Copy, PartialEq)]
    enum DriverType {
        Legacy,
        WinUsb,
        Unknown,
    }
    #[derive(Debug, Clone, Copy)]
    struct WinUsbEndpoints {
        read_pipe: u8,
        write_pipe: u8,
        max_packet_size: u16,
    }

    pub struct UsbTransport {
        device_handle: HANDLE,
        read_handle: HANDLE,
        write_handle: HANDLE,
        winusb_handle: WINUSB_INTERFACE_HANDLE,
        winusb_endpoints: Option<WinUsbEndpoints>,
        driver_type: DriverType,
        #[allow(dead_code)]
        serial_number: String,
        #[allow(dead_code)]
        device_path: String,
        #[allow(dead_code)]
        max_packet_size: usize,
        timeout: Duration,
        stats: TransportStats,
        #[allow(dead_code)]
        is_usb3: bool,
        write_event: HANDLE,
        read_event: HANDLE,
    }

    impl UsbTransport {
        pub fn enumerate_devices() -> Result<Vec<UsbDeviceInfo>, TransportError> {
            Self::enumerate_devices_by_mode(Some(UsbMode::Fastboot))
        }
        pub fn enumerate_adb_devices() -> Result<Vec<UsbDeviceInfo>, TransportError> {
            Self::enumerate_devices_by_mode(Some(UsbMode::Adb))
        }
        fn enumerate_devices_by_mode(
            mode_filter: Option<UsbMode>,
        ) -> Result<Vec<UsbDeviceInfo>, TransportError> {
            let mut devices = Vec::new();

            unsafe {
                let dev_info = SetupDiGetClassDevsW(
                    &ADB_DEVICE_GUID,
                    ptr::null(),
                    ptr::null_mut(),
                    DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
                );

                if dev_info == INVALID_HANDLE_VALUE {
                    return Ok(devices);
                }

                let mut dev_interface_data: SP_DEVICE_INTERFACE_DATA = mem::zeroed();
                dev_interface_data.cbSize = mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as DWORD;

                let mut index = 0;
                while SetupDiEnumDeviceInterfaces(
                    dev_info,
                    ptr::null_mut(),
                    &ADB_DEVICE_GUID,
                    index,
                    &mut dev_interface_data,
                ) != 0
                {
                    if let Some(info) = Self::get_device_info(dev_info, &mut dev_interface_data) {
                        let actual_mode = Self::detect_device_mode(&info);

                        match mode_filter {
                            Some(UsbMode::Fastboot) if actual_mode == Some(UsbMode::Fastboot) => {
                                devices.push(info)
                            }
                            Some(UsbMode::Adb) if actual_mode == Some(UsbMode::Adb) => {
                                devices.push(info)
                            }
                            None => devices.push(info),
                            _ => {}
                        }
                    }
                    index += 1;
                }

                SetupDiDestroyDeviceInfoList(dev_info);
            }
            devices.dedup_by(|a, b| a.serial_number == b.serial_number);

            Ok(devices)
        }
        fn detect_device_mode(info: &UsbDeviceInfo) -> Option<UsbMode> {
            if Self::is_fastboot_device(info) {
                return Some(UsbMode::Fastboot);
            }
            unsafe {
                let path: Vec<u16> = std::ffi::OsStr::new(&info.device_path)
                    .encode_wide()
                    .chain(std::iter::once(0))
                    .collect();

                let device_handle = CreateFileW(
                    path.as_ptr(),
                    GENERIC_READ,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    ptr::null_mut(),
                    OPEN_EXISTING,
                    FILE_FLAG_OVERLAPPED,
                    ptr::null_mut(),
                );

                if device_handle == INVALID_HANDLE_VALUE {
                    return Some(UsbMode::Adb);
                }

                let mut winusb_handle: WINUSB_INTERFACE_HANDLE = ptr::null_mut();
                if WinUsb_Initialize(device_handle, &mut winusb_handle) == 0 {
                    CloseHandle(device_handle);
                    return Some(UsbMode::Adb);
                }

                let mut iface_desc: USB_INTERFACE_DESCRIPTOR = mem::zeroed();
                if WinUsb_QueryInterfaceSettings(winusb_handle, 0, &mut iface_desc) == 0 {
                    WinUsb_Free(winusb_handle);
                    CloseHandle(device_handle);
                    return Some(UsbMode::Adb);
                }

                WinUsb_Free(winusb_handle);
                CloseHandle(device_handle);
                if iface_desc.bInterfaceClass == FASTBOOT_CLASS
                    && iface_desc.bInterfaceSubClass == FASTBOOT_SUBCLASS
                    && iface_desc.bInterfaceProtocol == FASTBOOT_PROTOCOL
                {
                    return Some(UsbMode::Fastboot);
                }
                if iface_desc.bInterfaceClass == FASTBOOT_CLASS
                    && iface_desc.bInterfaceSubClass == FASTBOOT_SUBCLASS
                    && iface_desc.bInterfaceProtocol == ADB_PROTOCOL
                {
                    return Some(UsbMode::Adb);
                }
                Some(UsbMode::Adb)
            }
        }
        fn is_fastboot_device(info: &UsbDeviceInfo) -> bool {
            const PURE_FASTBOOT_PIDS: &[u16] = &[
                0xd00d, 0x4ee0, 0x9008, 0x9006, 0x9025, 0x900e, 0xff40, 0x2281, 0x0fff, 0x6601,
                0x1057, 0x0016, 0x9011, 0x201c, 0x0003,
            ];
            if PURE_FASTBOOT_PIDS.contains(&info.product_id) {
                return true;
            }

            false
        }
        unsafe fn get_device_info(
            dev_info: HDEVINFO,
            dev_interface_data: &mut SP_DEVICE_INTERFACE_DATA,
        ) -> Option<UsbDeviceInfo> {
            let mut required_size: DWORD = 0;
            SetupDiGetDeviceInterfaceDetailW(
                dev_info,
                dev_interface_data,
                ptr::null_mut(),
                0,
                &mut required_size,
                ptr::null_mut(),
            );

            if required_size == 0 {
                return None;
            }
            let mut buffer = vec![0u8; required_size as usize];
            let detail_data = buffer.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
            (*detail_data).cbSize = mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as DWORD;
            if SetupDiGetDeviceInterfaceDetailW(
                dev_info,
                dev_interface_data,
                detail_data,
                required_size,
                ptr::null_mut(),
                ptr::null_mut(),
            ) == 0
            {
                return None;
            }
            let device_path_ptr = &(*detail_data).DevicePath as *const u16;
            let device_path_len = (0..)
                .take_while(|&i| *device_path_ptr.offset(i) != 0)
                .count();
            let device_path_slice = std::slice::from_raw_parts(device_path_ptr, device_path_len);
            let device_path = OsString::from_wide(device_path_slice)
                .to_string_lossy()
                .to_string();
            let (vid, pid) = Self::parse_vid_pid(&device_path)?;

            let serial = Self::extract_serial_from_path(&device_path)
                .or_else(|| Self::get_parent_serial_from_path(&device_path))
                .or_else(|| Self::get_serial_via_ioctl(&device_path))
                .unwrap_or_else(|| format!("{:04x}:{:04x}", vid, pid));

            Some(UsbDeviceInfo {
                vendor_id: vid,
                product_id: pid,
                serial_number: serial,
                device_path,
                is_usb3: false,
                product_name: Some("Android".to_string()),
                manufacturer: Some("Google, Inc".to_string()),
            })
        }
        fn parse_vid_pid(path: &str) -> Option<(u16, u16)> {
            let path_upper = path.to_uppercase();
            let vid_pos = path_upper.find("VID_")?;
            let pid_pos = path_upper.find("PID_")?;

            let vid_str = path.get(vid_pos + 4..vid_pos + 8)?;
            let pid_str = path.get(pid_pos + 4..pid_pos + 8)?;

            let vid = u16::from_str_radix(vid_str, 16).ok()?;
            let pid = u16::from_str_radix(pid_str, 16).ok()?;

            Some((vid, pid))
        }
        fn extract_serial_from_path(path: &str) -> Option<String> {
            let parts: Vec<&str> = path.split('#').collect();
            if parts.len() >= 3 {
                let serial = parts[2];
                if serial.starts_with('{') || serial.is_empty() || serial.len() >= 64 {
                    return None;
                }

                if serial.contains('&') {
                    return None;
                }
                return Some(serial.to_string());
            }
            None
        }
        fn get_serial_via_ioctl(device_path: &str) -> Option<String> {
            unsafe {
                let path: Vec<u16> = OsStr::new(device_path)
                    .encode_wide()
                    .chain(std::iter::once(0))
                    .collect();

                let handle = CreateFileW(
                    path.as_ptr(),
                    GENERIC_READ,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    ptr::null_mut(),
                    OPEN_EXISTING,
                    0,
                    ptr::null_mut(),
                );

                if handle == INVALID_HANDLE_VALUE {
                    return None;
                }

                let mut serial_buf = [0u16; 256];
                let mut bytes_returned: DWORD = 0;

                let result = DeviceIoControl(
                    handle,
                    ADB_IOCTL_GET_SERIAL_NUMBER,
                    ptr::null_mut(),
                    0,
                    serial_buf.as_mut_ptr() as *mut _,
                    (serial_buf.len() * 2) as DWORD,
                    &mut bytes_returned,
                    ptr::null_mut(),
                );

                CloseHandle(handle);

                if result != 0 && bytes_returned > 0 {
                    let len = serial_buf
                        .iter()
                        .position(|&c| c == 0)
                        .unwrap_or(serial_buf.len());
                    let serial = String::from_utf16_lossy(&serial_buf[..len]);
                    if !serial.is_empty() {
                        return Some(serial);
                    }
                }
            }
            None
        }

        fn get_parent_serial_from_path(device_path: &str) -> Option<String> {
            let path_lower = device_path.to_lowercase();
            if !path_lower.contains("&mi_") {
                return None;
            }

            let vid_pos = path_lower.find("vid_")?;
            let pid_pos = path_lower.find("pid_")?;

            let vid_str = device_path.get(vid_pos + 4..vid_pos + 8)?;
            let pid_end = path_lower[pid_pos + 4..]
                .find('&')
                .map(|p| pid_pos + 4 + p)
                .unwrap_or(pid_pos + 8);
            let pid_str = device_path.get(pid_pos + 4..pid_end)?;

            let reg_path = format!(
                "SYSTEM\\CurrentControlSet\\Enum\\USB\\VID_{}&PID_{}",
                vid_str.to_uppercase(),
                pid_str.to_uppercase()
            );
            unsafe {
                use winapi::um::winreg::*;

                let reg_path_wide: Vec<u16> = OsStr::new(&reg_path)
                    .encode_wide()
                    .chain(std::iter::once(0))
                    .collect();

                let mut hkey: winapi::shared::minwindef::HKEY = ptr::null_mut();
                let result = RegOpenKeyExW(
                    HKEY_LOCAL_MACHINE,
                    reg_path_wide.as_ptr(),
                    0,
                    KEY_READ,
                    &mut hkey,
                );

                if result != 0 {
                    return None;
                }
                let mut index = 0;
                let mut name_buf = [0u16; 256];
                let mut name_len = name_buf.len() as DWORD;

                while RegEnumKeyExW(
                    hkey,
                    index,
                    name_buf.as_mut_ptr(),
                    &mut name_len,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                ) == 0
                {
                    let name = String::from_utf16_lossy(&name_buf[..name_len as usize]);
                    if !name.is_empty() && !name.contains('&') && name.len() < 64 {
                        RegCloseKey(hkey);
                        return Some(name);
                    }

                    index += 1;
                    name_len = name_buf.len() as DWORD;
                }

                RegCloseKey(hkey);
            }

            None
        }
        pub fn open(serial: Option<&str>) -> Result<Self, TransportError> {
            Self::open_with_mode(serial, UsbMode::Fastboot)
        }
        pub fn open_adb(serial: Option<&str>) -> Result<Self, TransportError> {
            Self::open_with_mode(serial, UsbMode::Adb)
        }
        pub fn open_adb_by_info(info: &UsbDeviceInfo) -> Result<Self, TransportError> {
            Self::open_device_with_mode(info, UsbMode::Adb)
        }
        fn open_with_mode(serial: Option<&str>, mode: UsbMode) -> Result<Self, TransportError> {
            let devices = match mode {
                UsbMode::Fastboot => Self::enumerate_devices()?,
                UsbMode::Adb => Self::enumerate_adb_devices()?,
            };

            let device_info = match serial {
                Some(s) => devices
                    .into_iter()
                    .find(|d| d.serial_number == s)
                    .ok_or_else(|| TransportError::Usb(format!("设备 '{}' 未找到", s)))?,
                None => {
                    if devices.is_empty() {
                        return Err(TransportError::Usb("没有找到设备".into()));
                    }
                    if devices.len() > 1 {
                        return Err(TransportError::Usb(
                            "发现多个设备，请用 -s 指定序列号".into(),
                        ));
                    }
                    devices.into_iter().next().unwrap()
                }
            };

            Self::open_device_with_mode(&device_info, mode)
        }
        fn open_device_with_mode(
            info: &UsbDeviceInfo,
            mode: UsbMode,
        ) -> Result<Self, TransportError> {
            unsafe {
                let path: Vec<u16> = OsStr::new(&info.device_path)
                    .encode_wide()
                    .chain(std::iter::once(0))
                    .collect();

                let mut device_handle = CreateFileW(
                    path.as_ptr(),
                    GENERIC_READ,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    ptr::null_mut(),
                    OPEN_EXISTING,
                    FILE_FLAG_OVERLAPPED,
                    ptr::null_mut(),
                );

                if device_handle == INVALID_HANDLE_VALUE {
                    device_handle = CreateFileW(
                        path.as_ptr(),
                        0,
                        FILE_SHARE_READ | FILE_SHARE_WRITE,
                        ptr::null_mut(),
                        OPEN_EXISTING,
                        0,
                        ptr::null_mut(),
                    );

                    if device_handle == INVALID_HANDLE_VALUE {
                        return Err(TransportError::Usb(format!(
                            "无法打开设备: {}\n\
                             可能原因：\n\
                             1. 设备被其他程序占用\n\
                             2. 需要管理员权限\n\
                             3. 驱动未正确安装",
                            std::io::Error::last_os_error()
                        )));
                    }
                }
                let driver_type = Self::detect_driver_type(device_handle);
                CloseHandle(device_handle);

                match driver_type {
                    DriverType::Legacy => Self::open_legacy_device(info, &path),
                    DriverType::WinUsb => Self::open_winusb_device(info, &path, mode),
                    DriverType::Unknown => Self::open_winusb_device(info, &path, mode)
                        .or_else(|_| Self::open_legacy_device(info, &path))
                        .map_err(|_| {
                            TransportError::Usb(
                                "无法识别设备驱动类型，WinUSB 和 Legacy 模式都失败。\n\
                                 建议：安装 Google USB Driver。\n\
                                 下载地址: https://developer.android.com/studio/run/win-usb"
                                    .into(),
                            )
                        }),
                }
            }
        }
        fn open_legacy_device(info: &UsbDeviceInfo, path: &[u16]) -> Result<Self, TransportError> {
            unsafe {
                let read_path = format!("{}\\BulkRead", info.device_path);
                let write_path = format!("{}\\BulkWrite", info.device_path);
                let read_h = Self::open_endpoint(&read_path, true)?;

                let write_h = match Self::open_endpoint(&write_path, false) {
                    Ok(h) => h,
                    Err(e) => {
                        CloseHandle(read_h);
                        return Err(e);
                    }
                };
                let dev_h = CreateFileW(
                    path.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    ptr::null_mut(),
                    OPEN_EXISTING,
                    FILE_FLAG_OVERLAPPED,
                    ptr::null_mut(),
                );
                let write_event = CreateEventW(ptr::null_mut(), TRUE, FALSE, ptr::null());
                let read_event = CreateEventW(ptr::null_mut(), TRUE, FALSE, ptr::null());

                Ok(Self {
                    device_handle: dev_h,
                    read_handle: read_h,
                    write_handle: write_h,
                    winusb_handle: ptr::null_mut(),
                    winusb_endpoints: None,
                    driver_type: DriverType::Legacy,
                    serial_number: info.serial_number.clone(),
                    device_path: info.device_path.clone(),
                    max_packet_size: if info.is_usb3 {
                        USB3_MAX_PACKET_SIZE
                    } else {
                        USB2_MAX_PACKET_SIZE
                    },
                    timeout: DEFAULT_TIMEOUT,
                    stats: TransportStats::default(),
                    is_usb3: info.is_usb3,
                    write_event,
                    read_event,
                })
            }
        }
        fn open_winusb_device(
            info: &UsbDeviceInfo,
            path: &[u16],
            mode: UsbMode,
        ) -> Result<Self, TransportError> {
            let expected_protocol = match mode {
                UsbMode::Fastboot => FASTBOOT_PROTOCOL,
                UsbMode::Adb => ADB_PROTOCOL,
            };

            unsafe {
                let device_handle = CreateFileW(
                    path.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    ptr::null_mut(),
                    OPEN_EXISTING,
                    FILE_FLAG_OVERLAPPED,
                    ptr::null_mut(),
                );

                if device_handle == INVALID_HANDLE_VALUE {
                    return Err(TransportError::Usb(format!(
                        "WinUSB: 无法打开设备: {}",
                        std::io::Error::last_os_error()
                    )));
                }
                let mut winusb_handle: WINUSB_INTERFACE_HANDLE = ptr::null_mut();

                if WinUsb_Initialize(device_handle, &mut winusb_handle) == 0 {
                    let err = std::io::Error::last_os_error();
                    CloseHandle(device_handle);
                    return Err(TransportError::Usb(format!("WinUSB: 初始化失败: {}", err)));
                }
                let mut iface_desc: USB_INTERFACE_DESCRIPTOR = mem::zeroed();
                if WinUsb_QueryInterfaceSettings(winusb_handle, 0, &mut iface_desc) == 0 {
                    let err = std::io::Error::last_os_error();
                    WinUsb_Free(winusb_handle);
                    CloseHandle(device_handle);
                    return Err(TransportError::Usb(format!(
                        "WinUSB: 查询接口失败: {}",
                        err
                    )));
                }
                let mut read_pipe: u8 = 0;
                let mut write_pipe: u8 = 0;
                let mut max_packet_size: u16 = 512;

                let is_known_fastboot_pid = Self::is_fastboot_device(info);
                let protocol_ok = if mode == UsbMode::Fastboot && is_known_fastboot_pid {
                    iface_desc.bInterfaceClass == FASTBOOT_CLASS
                        && iface_desc.bInterfaceSubClass == FASTBOOT_SUBCLASS
                } else {
                    iface_desc.bInterfaceClass == FASTBOOT_CLASS
                        && iface_desc.bInterfaceSubClass == FASTBOOT_SUBCLASS
                        && iface_desc.bInterfaceProtocol == expected_protocol
                };

                if !protocol_ok {
                    WinUsb_Free(winusb_handle);
                    CloseHandle(device_handle);
                    let mode_name = match mode {
                        UsbMode::Fastboot => "Fastboot",
                        UsbMode::Adb => "ADB",
                    };
                    return Err(TransportError::Usb(format!(
                        "设备不是 {} 模式 (protocol={:02x}, 期望={:02x})",
                        mode_name, iface_desc.bInterfaceProtocol, expected_protocol
                    )));
                }

                for i in 0..iface_desc.bNumEndpoints {
                    let mut pipe_info: WINUSB_PIPE_INFORMATION = mem::zeroed();
                    if WinUsb_QueryPipe(winusb_handle, 0, i, &mut pipe_info) != 0 {
                        if pipe_info.PipeType == 2 {
                            if pipe_info.PipeId & USB_ENDPOINT_DIRECTION_IN != 0 {
                                read_pipe = pipe_info.PipeId;
                                max_packet_size = pipe_info.MaximumPacketSize;
                            } else {
                                write_pipe = pipe_info.PipeId;
                            }
                        }
                    }
                }
                if read_pipe == 0 {
                    read_pipe = 0x81;
                }
                if write_pipe == 0 {
                    write_pipe = 0x01;
                }
                let mut timeout_ms: DWORD = DEFAULT_TIMEOUT.as_millis() as DWORD;
                WinUsb_SetPipePolicy(
                    winusb_handle,
                    read_pipe,
                    PIPE_TRANSFER_TIMEOUT,
                    mem::size_of::<DWORD>() as DWORD,
                    &mut timeout_ms as *mut _ as *mut _,
                );
                WinUsb_SetPipePolicy(
                    winusb_handle,
                    write_pipe,
                    PIPE_TRANSFER_TIMEOUT,
                    mem::size_of::<DWORD>() as DWORD,
                    &mut timeout_ms as *mut _ as *mut _,
                );

                let endpoints = WinUsbEndpoints {
                    read_pipe,
                    write_pipe,
                    max_packet_size,
                };
                let write_event = CreateEventW(ptr::null_mut(), TRUE, FALSE, ptr::null());
                let read_event = CreateEventW(ptr::null_mut(), TRUE, FALSE, ptr::null());

                Ok(Self {
                    device_handle,
                    read_handle: INVALID_HANDLE_VALUE,
                    write_handle: INVALID_HANDLE_VALUE,
                    winusb_handle,
                    winusb_endpoints: Some(endpoints),
                    driver_type: DriverType::WinUsb,
                    serial_number: info.serial_number.clone(),
                    device_path: info.device_path.clone(),
                    max_packet_size: max_packet_size as usize,
                    timeout: DEFAULT_TIMEOUT,
                    stats: TransportStats::default(),
                    is_usb3: max_packet_size > 512,
                    write_event,
                    read_event,
                })
            }
        }
        fn detect_driver_type(handle: HANDLE) -> DriverType {
            unsafe {
                let mut desc: UsbDeviceDescriptor = mem::zeroed();
                let mut bytes_returned: DWORD = 0;

                let result = DeviceIoControl(
                    handle,
                    ADB_IOCTL_GET_USB_DEVICE_DESCRIPTOR,
                    ptr::null_mut(),
                    0,
                    &mut desc as *mut _ as *mut _,
                    mem::size_of::<UsbDeviceDescriptor>() as DWORD,
                    &mut bytes_returned,
                    ptr::null_mut(),
                );

                if result != 0 {
                    return DriverType::Legacy;
                }

                DriverType::WinUsb
            }
        }
        fn open_endpoint(path: &str, is_read: bool) -> Result<HANDLE, TransportError> {
            unsafe {
                let path_wide: Vec<u16> = OsStr::new(path)
                    .encode_wide()
                    .chain(std::iter::once(0))
                    .collect();

                let access = if is_read { GENERIC_READ } else { GENERIC_WRITE };

                let handle = CreateFileW(
                    path_wide.as_ptr(),
                    access,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    ptr::null_mut(),
                    OPEN_EXISTING,
                    FILE_FLAG_OVERLAPPED,
                    ptr::null_mut(),
                );

                if handle == INVALID_HANDLE_VALUE {
                    return Err(TransportError::Usb(format!(
                        "无法打开端点 {}: {}",
                        path,
                        std::io::Error::last_os_error()
                    )));
                }

                Ok(handle)
            }
        }

        pub fn stats(&self) -> &TransportStats {
            &self.stats
        }

        #[allow(dead_code)]
        pub fn is_usb3(&self) -> bool {
            self.is_usb3
        }
        pub fn bulk_write_sync(&mut self, data: &[u8]) -> Result<usize, TransportError> {
            if data.is_empty() {
                return Ok(0);
            }

            match self.driver_type {
                DriverType::Legacy => self.legacy_write(data),
                DriverType::WinUsb => self.winusb_write(data),
                DriverType::Unknown => self.legacy_write(data),
            }
        }

        fn winusb_write(&mut self, data: &[u8]) -> Result<usize, TransportError> {
            unsafe {
                let endpoints = self
                    .winusb_endpoints
                    .ok_or_else(|| TransportError::Usb("WinUSB 端点未初始化".into()))?;

                if self.winusb_handle.is_null() {
                    return Err(TransportError::Usb("WinUSB 句柄为空".into()));
                }

                let mut total_written = 0usize;
                let total_bytes = data.len() as f64;

                const MAX_CHUNK: usize = 1 * 1024 * 1024;

                while total_written < data.len() {
                    let chunk_size = std::cmp::min(MAX_CHUNK, data.len() - total_written);
                    let chunk = &data[total_written..total_written + chunk_size];
                    let mut overlapped: OVERLAPPED = mem::zeroed();
                    overlapped.hEvent = self.write_event;
                    ResetEvent(self.write_event);

                    let mut written: DWORD = 0;

                    let result = WinUsb_WritePipe(
                        self.winusb_handle,
                        endpoints.write_pipe,
                        chunk.as_ptr() as *mut u8,
                        chunk_size as DWORD,
                        &mut written,
                        &mut overlapped,
                    );
                    if result == 0 {
                        let err = GetLastError();
                        if err != ERROR_IO_PENDING {
                            return Err(TransportError::Usb(format!(
                                "WinUSB 写入失败: {}",
                                std::io::Error::from_raw_os_error(err as i32)
                            )));
                        }
                    }
                    let wait_result = WinUsb_GetOverlappedResult(
                        self.winusb_handle,
                        &mut overlapped,
                        &mut written,
                        TRUE,
                    );

                    if wait_result == 0 {
                        return Err(TransportError::Usb(format!(
                            "WinUSB 写入等待失败: {}",
                            std::io::Error::last_os_error()
                        )));
                    }

                    total_written += written as usize;
                }

                self.stats.bytes_sent += total_written as u64;
                self.stats.packets_sent += 1;

                Ok(total_written)
            }
        }

        fn legacy_write(&mut self, data: &[u8]) -> Result<usize, TransportError> {
            unsafe {
                let handle = if self.write_handle != INVALID_HANDLE_VALUE {
                    self.write_handle
                } else {
                    self.device_handle
                };

                let mut total_written = 0usize;
                let total_bytes = data.len() as f64;
                let timeout_ms = self.timeout.as_millis() as DWORD;
                const MAX_CHUNK: usize = 1 * 1024 * 1024;

                while total_written < data.len() {
                    let chunk_size = std::cmp::min(MAX_CHUNK, data.len() - total_written);
                    let chunk = &data[total_written..total_written + chunk_size];
                    let mut overlapped: OVERLAPPED = mem::zeroed();
                    overlapped.hEvent = self.write_event;
                    ResetEvent(self.write_event);

                    let mut written: DWORD = 0;
                    let result = WriteFile(
                        handle,
                        chunk.as_ptr() as *const _,
                        chunk_size as DWORD,
                        &mut written,
                        &mut overlapped,
                    );

                    if result == 0 {
                        let err = GetLastError();
                        if err != ERROR_IO_PENDING {
                            return Err(TransportError::Usb(format!(
                                "写入失败: {}",
                                std::io::Error::from_raw_os_error(err as i32)
                            )));
                        }
                        let wait_result = WaitForSingleObject(overlapped.hEvent, timeout_ms);
                        if wait_result != WAIT_OBJECT_0 {
                            CancelIo(handle);
                            return Err(TransportError::Timeout);
                        }
                        if GetOverlappedResult(handle, &mut overlapped, &mut written, FALSE) == 0 {
                            return Err(TransportError::Usb(format!(
                                "获取写入结果失败: {}",
                                std::io::Error::last_os_error()
                            )));
                        }
                    }

                    total_written += written as usize;
                }

                self.stats.bytes_sent += total_written as u64;
                self.stats.packets_sent += 1;

                Ok(total_written)
            }
        }
        #[allow(dead_code)]
        fn direct_write(&mut self, data: &[u8]) -> Result<usize, TransportError> {
            if self.driver_type == DriverType::WinUsb {
                self.winusb_write(data)
            } else {
                self.legacy_write(data)
            }
        }
        pub fn bulk_read_sync(&mut self, buf: &mut [u8]) -> Result<usize, TransportError> {
            if buf.is_empty() {
                return Ok(0);
            }

            match self.driver_type {
                DriverType::Legacy => self.legacy_read(buf),
                DriverType::WinUsb => self.winusb_read(buf),
                DriverType::Unknown => self.legacy_read(buf),
            }
        }

        fn winusb_read(&mut self, buf: &mut [u8]) -> Result<usize, TransportError> {
            unsafe {
                let endpoints = self
                    .winusb_endpoints
                    .ok_or_else(|| TransportError::Usb("WinUSB 端点未初始化".into()))?;

                if self.winusb_handle.is_null() {
                    return Err(TransportError::Usb("WinUSB 句柄为空".into()));
                }
                let mut overlapped: OVERLAPPED = mem::zeroed();
                overlapped.hEvent = self.read_event;
                ResetEvent(self.read_event);

                let mut read: DWORD = 0;

                let result = WinUsb_ReadPipe(
                    self.winusb_handle,
                    endpoints.read_pipe,
                    buf.as_mut_ptr(),
                    buf.len() as DWORD,
                    &mut read,
                    &mut overlapped,
                );
                if result == 0 {
                    let err = GetLastError();
                    if err != ERROR_IO_PENDING {
                        return Err(TransportError::Usb(format!(
                            "WinUSB 读取失败: {}",
                            std::io::Error::from_raw_os_error(err as i32)
                        )));
                    }
                }
                let timeout_ms = self.timeout.as_millis() as DWORD;
                let actual_timeout = if timeout_ms == 0 {
                    INFINITE
                } else {
                    timeout_ms
                };

                let wait_result = WaitForSingleObject(overlapped.hEvent, actual_timeout);

                if wait_result == WAIT_TIMEOUT {
                    WinUsb_AbortPipe(self.winusb_handle, endpoints.read_pipe);
                    return Err(TransportError::Timeout);
                }

                if wait_result != WAIT_OBJECT_0 {
                    return Err(TransportError::Usb(format!(
                        "WinUSB 等待失败: {}",
                        std::io::Error::last_os_error()
                    )));
                }
                let get_result = WinUsb_GetOverlappedResult(
                    self.winusb_handle,
                    &mut overlapped,
                    &mut read,
                    FALSE,
                );

                if get_result == 0 {
                    let err = GetLastError();
                    return Err(TransportError::Usb(format!(
                        "winusb read error: {} (code: {})",
                        std::io::Error::last_os_error(),
                        err
                    )));
                }

                self.stats.bytes_received += read as u64;
                self.stats.packets_received += 1;

                Ok(read as usize)
            }
        }

        fn legacy_read(&mut self, buf: &mut [u8]) -> Result<usize, TransportError> {
            unsafe {
                let handle = if self.read_handle != INVALID_HANDLE_VALUE {
                    self.read_handle
                } else {
                    self.device_handle
                };
                let mut overlapped: OVERLAPPED = mem::zeroed();
                overlapped.hEvent = self.read_event;
                ResetEvent(self.read_event);

                let mut read: DWORD = 0;
                let timeout_ms = self.timeout.as_millis() as DWORD;
                let actual_timeout = if timeout_ms == 0 {
                    INFINITE
                } else {
                    timeout_ms
                };

                let result = ReadFile(
                    handle,
                    buf.as_mut_ptr() as *mut _,
                    buf.len() as DWORD,
                    &mut read,
                    &mut overlapped,
                );

                if result == 0 {
                    let err = GetLastError();
                    if err != ERROR_IO_PENDING {
                        return Err(TransportError::Usb(format!(
                            "读取失败: {}",
                            std::io::Error::from_raw_os_error(err as i32)
                        )));
                    }
                    let wait_result = WaitForSingleObject(overlapped.hEvent, actual_timeout);
                    if wait_result != WAIT_OBJECT_0 {
                        CancelIo(handle);
                        if wait_result == WAIT_TIMEOUT {
                            return Err(TransportError::Timeout);
                        }
                        return Err(TransportError::Usb("等待读取失败".into()));
                    }
                    if GetOverlappedResult(handle, &mut overlapped, &mut read, FALSE) == 0 {
                        return Err(TransportError::Usb(format!(
                            "获取读取结果失败: {}",
                            std::io::Error::last_os_error()
                        )));
                    }
                }

                self.stats.bytes_received += read as u64;
                self.stats.packets_received += 1;

                Ok(read as usize)
            }
        }
        #[allow(dead_code)]
        fn direct_read(&mut self, buf: &mut [u8]) -> Result<usize, TransportError> {
            if self.driver_type == DriverType::WinUsb {
                self.winusb_read(buf)
            } else {
                self.legacy_read(buf)
            }
        }

        pub async fn bulk_write(&mut self, data: &[u8]) -> Result<usize, TransportError> {
            self.bulk_write_sync(data)
        }

        pub async fn bulk_read(&mut self, buf: &mut [u8]) -> Result<usize, TransportError> {
            self.bulk_read_sync(buf)
        }
        pub fn set_timeout(&mut self, timeout: Duration) {
            self.timeout = timeout;
            if self.driver_type == DriverType::WinUsb && !self.winusb_handle.is_null() {
                if let Some(endpoints) = self.winusb_endpoints {
                    unsafe {
                        let mut timeout_ms: DWORD = timeout.as_millis() as DWORD;
                        WinUsb_SetPipePolicy(
                            self.winusb_handle,
                            endpoints.read_pipe,
                            PIPE_TRANSFER_TIMEOUT,
                            mem::size_of::<DWORD>() as DWORD,
                            &mut timeout_ms as *mut _ as *mut _,
                        );
                        WinUsb_SetPipePolicy(
                            self.winusb_handle,
                            endpoints.write_pipe,
                            PIPE_TRANSFER_TIMEOUT,
                            mem::size_of::<DWORD>() as DWORD,
                            &mut timeout_ms as *mut _ as *mut _,
                        );
                    }
                }
            }
        }
    }

    impl Drop for UsbTransport {
        fn drop(&mut self) {
            unsafe {
                if !self.winusb_handle.is_null() {
                    WinUsb_Free(self.winusb_handle);
                }
                if !self.write_event.is_null() {
                    CloseHandle(self.write_event);
                }
                if !self.read_event.is_null() {
                    CloseHandle(self.read_event);
                }
                if self.write_handle != INVALID_HANDLE_VALUE {
                    CloseHandle(self.write_handle);
                }
                if self.read_handle != INVALID_HANDLE_VALUE {
                    CloseHandle(self.read_handle);
                }
                if self.device_handle != INVALID_HANDLE_VALUE {
                    CloseHandle(self.device_handle);
                }
            }
        }
    }

    impl AsyncTransport for UsbTransport {
        fn read<'a>(
            &'a mut self,
            buf: &'a mut [u8],
        ) -> Pin<Box<dyn Future<Output = Result<usize, TransportError>> + Send + 'a>> {
            Box::pin(async move { self.bulk_read(buf).await })
        }

        fn write<'a>(
            &'a mut self,
            data: &'a [u8],
        ) -> Pin<Box<dyn Future<Output = Result<usize, TransportError>> + Send + 'a>> {
            Box::pin(async move { self.bulk_write(data).await })
        }

        fn close(
            &mut self,
        ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>> {
            Box::pin(async move { Ok(()) })
        }

        fn reset(
            &mut self,
        ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>> {
            Box::pin(async move {
                unsafe {
                    if !self.winusb_handle.is_null() {
                        if let Some(endpoints) = self.winusb_endpoints {
                            let _ = WinUsb_AbortPipe(self.winusb_handle, endpoints.read_pipe);
                            let _ = WinUsb_AbortPipe(self.winusb_handle, endpoints.write_pipe);
                            let _ = WinUsb_FlushPipe(self.winusb_handle, endpoints.read_pipe);
                            let _ = WinUsb_FlushPipe(self.winusb_handle, endpoints.write_pipe);
                            let _ = WinUsb_ResetPipe(self.winusb_handle, endpoints.read_pipe);
                            let _ = WinUsb_ResetPipe(self.winusb_handle, endpoints.write_pipe);
                        }
                    }
                }
                Ok(())
            })
        }

        fn reinitialize(
            &mut self,
        ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>> {
            Box::pin(async move {
                if self.driver_type != DriverType::WinUsb {
                    return Ok(());
                }

                unsafe {
                    let device_path = self.device_path.clone();
                    let timeout = self.timeout;
                    let endpoints = self.winusb_endpoints;
                    if !self.winusb_handle.is_null() {
                        if let Some(ep) = endpoints {
                            let _ = WinUsb_AbortPipe(self.winusb_handle, ep.read_pipe);
                            let _ = WinUsb_AbortPipe(self.winusb_handle, ep.write_pipe);
                        }
                    }
                    if !self.winusb_handle.is_null() {
                        WinUsb_Free(self.winusb_handle);
                        self.winusb_handle = ptr::null_mut();
                    }
                    if self.device_handle != INVALID_HANDLE_VALUE {
                        CloseHandle(self.device_handle);
                        self.device_handle = INVALID_HANDLE_VALUE;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    let path: Vec<u16> = std::ffi::OsStr::new(&device_path)
                        .encode_wide()
                        .chain(std::iter::once(0))
                        .collect();

                    let device_handle = CreateFileW(
                        path.as_ptr(),
                        GENERIC_READ | GENERIC_WRITE,
                        FILE_SHARE_READ | FILE_SHARE_WRITE,
                        ptr::null_mut(),
                        OPEN_EXISTING,
                        FILE_FLAG_OVERLAPPED,
                        ptr::null_mut(),
                    );

                    if device_handle == INVALID_HANDLE_VALUE {
                        return Err(TransportError::Usb(format!(
                            "重新打开设备失败: {}",
                            std::io::Error::last_os_error()
                        )));
                    }

                    self.device_handle = device_handle;
                    let mut winusb_handle: WINUSB_INTERFACE_HANDLE = ptr::null_mut();
                    if WinUsb_Initialize(device_handle, &mut winusb_handle) == 0 {
                        let err = std::io::Error::last_os_error();
                        CloseHandle(device_handle);
                        self.device_handle = INVALID_HANDLE_VALUE;
                        return Err(TransportError::Usb(format!(
                            "重新初始化 WinUSB 失败: {}",
                            err
                        )));
                    }

                    self.winusb_handle = winusb_handle;
                    if let Some(ep) = endpoints {
                        let mut timeout_ms: DWORD = timeout.as_millis() as DWORD;
                        WinUsb_SetPipePolicy(
                            winusb_handle,
                            ep.read_pipe,
                            PIPE_TRANSFER_TIMEOUT,
                            mem::size_of::<DWORD>() as DWORD,
                            &mut timeout_ms as *mut _ as *mut _,
                        );
                        WinUsb_SetPipePolicy(
                            winusb_handle,
                            ep.write_pipe,
                            PIPE_TRANSFER_TIMEOUT,
                            mem::size_of::<DWORD>() as DWORD,
                            &mut timeout_ms as *mut _ as *mut _,
                        );
                    }
                }

                Ok(())
            })
        }

        fn set_timeout(&mut self, timeout: Duration) {
            self.timeout = timeout;
            if self.driver_type == DriverType::WinUsb && !self.winusb_handle.is_null() {
                if let Some(endpoints) = self.winusb_endpoints {
                    let mut timeout_ms: DWORD = timeout.as_millis() as DWORD;
                    unsafe {
                        let _ = WinUsb_SetPipePolicy(
                            self.winusb_handle,
                            endpoints.read_pipe,
                            PIPE_TRANSFER_TIMEOUT,
                            std::mem::size_of::<DWORD>() as DWORD,
                            &mut timeout_ms as *mut _ as *mut _,
                        );
                        let _ = WinUsb_SetPipePolicy(
                            self.winusb_handle,
                            endpoints.write_pipe,
                            PIPE_TRANSFER_TIMEOUT,
                            std::mem::size_of::<DWORD>() as DWORD,
                            &mut timeout_ms as *mut _ as *mut _,
                        );
                    }
                }
            }
        }

        fn max_packet_size(&self) -> usize {
            self.max_packet_size
        }

        fn supports_bulk_optimization(&self) -> bool {
            true
        }
    }

    unsafe impl Send for UsbTransport {}
    unsafe impl Sync for UsbTransport {}
}

#[cfg(not(target_os = "windows"))]
pub use self::unix_impl::*;

#[cfg(not(target_os = "windows"))]
mod unix_impl {
    use super::*;
    use rusb::{Context, Device, DeviceHandle, Direction, TransferType, UsbContext};

    pub struct UsbTransport {
        handle: DeviceHandle<Context>,
        interface_number: u8,
        read_endpoint: u8,
        write_endpoint: u8,
        max_packet_size: usize,
        timeout: Duration,
        stats: TransportStats,
        is_usb3: bool,
        kernel_driver_detached: bool,
    }

    impl UsbTransport {
        pub fn enumerate_devices() -> Result<Vec<UsbDeviceInfo>, TransportError> {
            let context = Context::new().map_err(|e| TransportError::Usb(e.to_string()))?;
            let mut devices = Vec::new();

            for device in context
                .devices()
                .map_err(|e| TransportError::Usb(e.to_string()))?
                .iter()
            {
                if let Some(info) = Self::check_fastboot_device(&device) {
                    devices.push(info);
                }
            }

            Ok(devices)
        }

        fn check_fastboot_device(device: &Device<Context>) -> Option<UsbDeviceInfo> {
            let desc = device.device_descriptor().ok()?;
            let vid = desc.vendor_id();
            let pid = desc.product_id();

            for config_idx in 0..desc.num_configurations() {
                let config = device.config_descriptor(config_idx).ok()?;
                for iface in config.interfaces() {
                    for iface_desc in iface.descriptors() {
                        if iface_desc.class_code() == FASTBOOT_CLASS
                            && iface_desc.sub_class_code() == FASTBOOT_SUBCLASS
                            && iface_desc.protocol_code() == FASTBOOT_PROTOCOL
                        {
                            let handle = device.open().ok();
                            let serial = handle
                                .as_ref()
                                .and_then(|h| h.read_serial_number_string_ascii(&desc).ok())
                                .unwrap_or_else(|| format!("{:04x}:{:04x}", vid, pid));
                            let product_name = handle
                                .as_ref()
                                .and_then(|h| h.read_product_string_ascii(&desc).ok());
                            let manufacturer = handle
                                .as_ref()
                                .and_then(|h| h.read_manufacturer_string_ascii(&desc).ok());
                            let is_usb3 = iface_desc
                                .endpoint_descriptors()
                                .any(|ep| ep.max_packet_size() > 512);

                            return Some(UsbDeviceInfo {
                                vendor_id: vid,
                                product_id: pid,
                                serial_number: serial,
                                device_path: format!(
                                    "{}:{}",
                                    device.bus_number(),
                                    device.address()
                                ),
                                is_usb3,
                                product_name,
                                manufacturer,
                            });
                        }
                    }
                }
            }
            None
        }
        fn check_adb_device(device: &Device<Context>) -> Option<UsbDeviceInfo> {
            let desc = device.device_descriptor().ok()?;
            let vid = desc.vendor_id();
            let pid = desc.product_id();

            for config_idx in 0..desc.num_configurations() {
                let config = device.config_descriptor(config_idx).ok()?;
                for iface in config.interfaces() {
                    for iface_desc in iface.descriptors() {
                        if iface_desc.class_code() == FASTBOOT_CLASS
                            && iface_desc.sub_class_code() == FASTBOOT_SUBCLASS
                            && iface_desc.protocol_code() == ADB_PROTOCOL
                        {
                            let handle = device.open().ok();
                            let serial = handle
                                .as_ref()
                                .and_then(|h| h.read_serial_number_string_ascii(&desc).ok())
                                .unwrap_or_else(|| format!("{:04x}:{:04x}", vid, pid));
                            let product_name = handle
                                .as_ref()
                                .and_then(|h| h.read_product_string_ascii(&desc).ok());
                            let manufacturer = handle
                                .as_ref()
                                .and_then(|h| h.read_manufacturer_string_ascii(&desc).ok());
                            let is_usb3 = iface_desc
                                .endpoint_descriptors()
                                .any(|ep| ep.max_packet_size() > 512);

                            return Some(UsbDeviceInfo {
                                vendor_id: vid,
                                product_id: pid,
                                serial_number: serial,
                                device_path: format!(
                                    "{}:{}",
                                    device.bus_number(),
                                    device.address()
                                ),
                                is_usb3,
                                product_name,
                                manufacturer,
                            });
                        }
                    }
                }
            }
            None
        }
        pub fn enumerate_adb_devices() -> Result<Vec<UsbDeviceInfo>, TransportError> {
            let context = Context::new().map_err(|e| TransportError::Usb(e.to_string()))?;
            let mut devices = Vec::new();

            for device in context
                .devices()
                .map_err(|e| TransportError::Usb(e.to_string()))?
                .iter()
            {
                if let Some(info) = Self::check_adb_device(&device) {
                    devices.push(info);
                }
            }

            Ok(devices)
        }
        pub fn open_adb(serial: Option<&str>) -> Result<Self, TransportError> {
            let devices = Self::enumerate_adb_devices()?;

            let device_info = match serial {
                Some(s) => devices
                    .into_iter()
                    .find(|d| d.serial_number == s)
                    .ok_or_else(|| TransportError::Usb(format!("设备 '{}' 未找到", s)))?,
                None => {
                    if devices.is_empty() {
                        return Err(TransportError::Usb("无设备".into()));
                    }
                    if devices.len() > 1 {
                        return Err(TransportError::Usb("多设备，请使用 -s 指定".into()));
                    }
                    devices.into_iter().next().unwrap()
                }
            };

            Self::open_adb_device(&device_info)
        }
        fn open_adb_device(info: &UsbDeviceInfo) -> Result<Self, TransportError> {
            let context = Context::new().map_err(|e| TransportError::Usb(e.to_string()))?;
            let parts: Vec<&str> = info.device_path.split(':').collect();
            let bus: u8 = parts.get(0).and_then(|s| s.parse().ok()).unwrap_or(0);
            let addr: u8 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);

            let device = context
                .devices()
                .map_err(|e| TransportError::Usb(e.to_string()))?
                .iter()
                .find(|d| d.bus_number() == bus && d.address() == addr)
                .ok_or_else(|| TransportError::Usb("Device disconnected".into()))?;

            let mut handle = device
                .open()
                .map_err(|e| TransportError::Usb(format!("Cannot open device: {}", e)))?;

            let (iface_num, read_ep, write_ep) = Self::find_adb_interface(&device)?;

            let mut kernel_driver_detached = false;
            if handle.kernel_driver_active(iface_num).unwrap_or(false) {
                handle.detach_kernel_driver(iface_num).map_err(|e| {
                    TransportError::Usb(format!("Cannot detach kernel driver: {}", e))
                })?;
                kernel_driver_detached = true;
            }

            handle
                .claim_interface(iface_num)
                .map_err(|e| TransportError::Usb(format!("Cannot claim interface: {}", e)))?;

            Ok(Self {
                handle,
                interface_number: iface_num,
                read_endpoint: read_ep,
                write_endpoint: write_ep,
                max_packet_size: if info.is_usb3 {
                    USB3_MAX_PACKET_SIZE
                } else {
                    USB2_MAX_PACKET_SIZE
                },
                timeout: DEFAULT_TIMEOUT,
                stats: TransportStats::default(),
                is_usb3: info.is_usb3,
                kernel_driver_detached,
            })
        }
        fn find_adb_interface(device: &Device<Context>) -> Result<(u8, u8, u8), TransportError> {
            let desc = device
                .device_descriptor()
                .map_err(|e| TransportError::Usb(e.to_string()))?;

            for config_idx in 0..desc.num_configurations() {
                let config = device
                    .config_descriptor(config_idx)
                    .map_err(|e| TransportError::Usb(e.to_string()))?;

                for iface in config.interfaces() {
                    for iface_desc in iface.descriptors() {
                        if iface_desc.class_code() == FASTBOOT_CLASS
                            && iface_desc.sub_class_code() == FASTBOOT_SUBCLASS
                            && iface_desc.protocol_code() == ADB_PROTOCOL
                        {
                            let mut read_ep = None;
                            let mut write_ep = None;

                            for ep in iface_desc.endpoint_descriptors() {
                                if ep.transfer_type() == TransferType::Bulk {
                                    match ep.direction() {
                                        Direction::In => read_ep = Some(ep.address()),
                                        Direction::Out => write_ep = Some(ep.address()),
                                    }
                                }
                            }

                            if let (Some(r), Some(w)) = (read_ep, write_ep) {
                                return Ok((iface_desc.interface_number(), r, w));
                            }
                        }
                    }
                }
            }

            Err(TransportError::Usb("接口未找到".into()))
        }

        pub fn open(serial: Option<&str>) -> Result<Self, TransportError> {
            let devices = Self::enumerate_devices()?;

            let device_info = match serial {
                Some(s) => devices
                    .into_iter()
                    .find(|d| d.serial_number == s)
                    .ok_or_else(|| TransportError::Usb(format!("Device '{}' not found", s)))?,
                None => {
                    if devices.is_empty() {
                        return Err(TransportError::Usb("No fastboot devices found".into()));
                    }
                    if devices.len() > 1 {
                        return Err(TransportError::Usb(
                            "Multiple devices found, use -s to specify".into(),
                        ));
                    }
                    devices.into_iter().next().unwrap()
                }
            };

            Self::open_device(&device_info)
        }

        fn open_device(info: &UsbDeviceInfo) -> Result<Self, TransportError> {
            let context = Context::new().map_err(|e| TransportError::Usb(e.to_string()))?;
            let parts: Vec<&str> = info.device_path.split(':').collect();
            let bus: u8 = parts.get(0).and_then(|s| s.parse().ok()).unwrap_or(0);
            let addr: u8 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);

            let device = context
                .devices()
                .map_err(|e| TransportError::Usb(e.to_string()))?
                .iter()
                .find(|d| d.bus_number() == bus && d.address() == addr)
                .ok_or_else(|| TransportError::Usb("Device disconnected".into()))?;

            let mut handle = device
                .open()
                .map_err(|e| TransportError::Usb(format!("Cannot open device: {}", e)))?;

            let (iface_num, read_ep, write_ep) = Self::find_fastboot_interface(&device)?;

            let mut kernel_driver_detached = false;
            if handle.kernel_driver_active(iface_num).unwrap_or(false) {
                handle.detach_kernel_driver(iface_num).map_err(|e| {
                    TransportError::Usb(format!("Cannot detach kernel driver: {}", e))
                })?;
                kernel_driver_detached = true;
            }

            handle
                .claim_interface(iface_num)
                .map_err(|e| TransportError::Usb(format!("Cannot claim interface: {}", e)))?;

            Ok(Self {
                handle,
                interface_number: iface_num,
                read_endpoint: read_ep,
                write_endpoint: write_ep,
                max_packet_size: if info.is_usb3 {
                    USB3_MAX_PACKET_SIZE
                } else {
                    USB2_MAX_PACKET_SIZE
                },
                timeout: DEFAULT_TIMEOUT,
                stats: TransportStats::default(),
                is_usb3: info.is_usb3,
                kernel_driver_detached,
            })
        }

        fn find_fastboot_interface(
            device: &Device<Context>,
        ) -> Result<(u8, u8, u8), TransportError> {
            let desc = device
                .device_descriptor()
                .map_err(|e| TransportError::Usb(e.to_string()))?;

            for config_idx in 0..desc.num_configurations() {
                let config = device
                    .config_descriptor(config_idx)
                    .map_err(|e| TransportError::Usb(e.to_string()))?;

                for iface in config.interfaces() {
                    for iface_desc in iface.descriptors() {
                        if iface_desc.class_code() == FASTBOOT_CLASS
                            && iface_desc.sub_class_code() == FASTBOOT_SUBCLASS
                            && iface_desc.protocol_code() == FASTBOOT_PROTOCOL
                        {
                            let mut read_ep = None;
                            let mut write_ep = None;

                            for ep in iface_desc.endpoint_descriptors() {
                                if ep.transfer_type() == TransferType::Bulk {
                                    match ep.direction() {
                                        Direction::In => read_ep = Some(ep.address()),
                                        Direction::Out => write_ep = Some(ep.address()),
                                    }
                                }
                            }

                            if let (Some(r), Some(w)) = (read_ep, write_ep) {
                                return Ok((iface_desc.interface_number(), r, w));
                            }
                        }
                    }
                }
            }

            Err(TransportError::Usb("Fastboot interface not found".into()))
        }

        pub fn stats(&self) -> &TransportStats {
            &self.stats
        }

        pub fn is_usb3(&self) -> bool {
            self.is_usb3
        }

        pub fn set_timeout(&mut self, timeout: Duration) {
            self.timeout = timeout;
        }

        pub fn bulk_write_sync(&mut self, data: &[u8]) -> Result<usize, TransportError> {
            let written = self
                .handle
                .write_bulk(self.write_endpoint, data, self.timeout)
                .map_err(|e| TransportError::Usb(format!("Write failed: {}", e)))?;
            self.stats.bytes_sent += written as u64;
            self.stats.packets_sent += 1;
            Ok(written)
        }

        pub fn bulk_read_sync(&mut self, buf: &mut [u8]) -> Result<usize, TransportError> {
            let read = self
                .handle
                .read_bulk(self.read_endpoint, buf, self.timeout)
                .map_err(|e| TransportError::Usb(format!("Read failed: {}", e)))?;
            self.stats.bytes_received += read as u64;
            self.stats.packets_received += 1;
            Ok(read)
        }

        pub async fn bulk_write(&mut self, data: &[u8]) -> Result<usize, TransportError> {
            self.bulk_write_sync(data)
        }

        pub async fn bulk_read(&mut self, buf: &mut [u8]) -> Result<usize, TransportError> {
            self.bulk_read_sync(buf)
        }
    }

    impl Drop for UsbTransport {
        fn drop(&mut self) {
            let _ = self.handle.release_interface(self.interface_number);
            if self.kernel_driver_detached {
                let _ = self.handle.attach_kernel_driver(self.interface_number);
            }
        }
    }

    impl AsyncTransport for UsbTransport {
        fn read<'a>(
            &'a mut self,
            buf: &'a mut [u8],
        ) -> Pin<Box<dyn Future<Output = Result<usize, TransportError>> + Send + 'a>> {
            Box::pin(async move { self.bulk_read(buf).await })
        }

        fn write<'a>(
            &'a mut self,
            data: &'a [u8],
        ) -> Pin<Box<dyn Future<Output = Result<usize, TransportError>> + Send + 'a>> {
            Box::pin(async move { self.bulk_write(data).await })
        }

        fn close(
            &mut self,
        ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>> {
            Box::pin(async move { Ok(()) })
        }

        fn reset(
            &mut self,
        ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>> {
            Box::pin(async move {
                self.handle
                    .reset()
                    .map_err(|e| TransportError::Usb(format!("Reset failed: {}", e)))
            })
        }

        fn max_packet_size(&self) -> usize {
            self.max_packet_size
        }

        fn supports_bulk_optimization(&self) -> bool {
            true
        }
    }

    unsafe impl Send for UsbTransport {}
    unsafe impl Sync for UsbTransport {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_constants() {
        assert_eq!(FASTBOOT_CLASS, 0xFF);
        assert_eq!(FASTBOOT_SUBCLASS, 0x42);
        assert_eq!(FASTBOOT_PROTOCOL, 0x03);
    }

    #[tokio::test]
    async fn test_enumerate_no_panic() {
        let result = UsbTransport::enumerate_devices();
        assert!(result.is_ok());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn test_parse_vid_pid() {
        use windows_impl::UsbTransport;

        let path = r"\\?\usb#vid_18d1&pid_d00d#abc123#{f72fe0d4-cbcb-407d-8814-9ed673d0dd6b}";
        let result = UsbTransport::parse_vid_pid(path);
        assert_eq!(result, Some((0x18d1, 0xd00d)));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn test_extract_serial() {
        use windows_impl::UsbTransport;

        let path = r"\\?\usb#vid_18d1&pid_d00d#abc123#{f72fe0d4-cbcb-407d-8814-9ed673d0dd6b}";
        let result = UsbTransport::extract_serial_from_path(path);
        assert_eq!(result, Some("abc123".to_string()));
    }
}
