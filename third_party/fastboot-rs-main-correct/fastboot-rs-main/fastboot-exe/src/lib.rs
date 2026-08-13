#![allow(dead_code)]

mod adb;
mod adb_protocol;
mod adb_winusb_transport;
mod cli;
pub mod crypto;
mod driver;
mod error;
mod flash;
mod logger;
mod partition;
mod progress;
mod protocol;
mod sparse;
mod tcp_transport;
mod transport;
mod udp_transport;
mod usb_transport;
mod util;

use std::ffi::{CStr, CString};
use std::fs;
use std::os::raw::c_char;
use std::path::Path;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use error::FastbootError;
use usb_transport::UsbTransport;
static INITIALIZED: AtomicBool = AtomicBool::new(false);
static mut RUNTIME: Option<tokio::runtime::Runtime> = None;
const VALID_TOKEN: u64 = 0xFB00_2024_0103;
pub type ProgressCallback = extern "C" fn(current: u64, total: u64, status: *const c_char);
pub type OutputCallback = extern "C" fn(line: *const c_char);

#[no_mangle]
pub extern "C" fn fastboot_init(token: u64) -> i32 {
    if token != VALID_TOKEN {
        return -1;
    }
    if INITIALIZED.load(Ordering::SeqCst) {
        return -2;
    }

    unsafe {
        RUNTIME = Some(
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("Failed to create runtime"),
        );
    }
    INITIALIZED.store(true, Ordering::SeqCst);
    0
}
#[no_mangle]
pub extern "C" fn fastboot_cleanup() {
    if INITIALIZED.load(Ordering::SeqCst) {
        unsafe {
            RUNTIME = None;
        }
        INITIALIZED.store(false, Ordering::SeqCst);
    }
}

macro_rules! check_init {
    () => {
        if !INITIALIZED.load(Ordering::SeqCst) {
            return -1;
        }
    };
}

macro_rules! get_runtime {
    () => {
        unsafe { RUNTIME.as_ref().unwrap() }
    };
}

unsafe fn ptr_to_option_str<'a>(ptr: *const c_char) -> Option<&'a str> {
    if ptr.is_null() {
        None
    } else {
        CStr::from_ptr(ptr).to_str().ok()
    }
}

unsafe fn ptr_to_str<'a>(ptr: *const c_char) -> &'a str {
    if ptr.is_null() {
        ""
    } else {
        CStr::from_ptr(ptr).to_str().unwrap_or("")
    }
}

fn write_to_buffer(s: &str, buf: *mut c_char, buf_len: usize) -> i32 {
    if buf.is_null() || buf_len == 0 {
        return s.len() as i32;
    }
    if s.len() >= buf_len {
        return -2;
    }
    unsafe {
        ptr::copy_nonoverlapping(s.as_ptr(), buf as *mut u8, s.len());
        *buf.add(s.len()) = 0;
    }
    s.len() as i32
}

#[no_mangle]
pub extern "C" fn fastboot_devices(out_buf: *mut c_char, buf_len: usize) -> i32 {
    check_init!();
    let result = get_devices_string();
    write_to_buffer(&result, out_buf, buf_len)
}

fn get_devices_string() -> String {
    use adb::client::AdbClient;
    let mut lines = Vec::new();

    if let Ok(devices) = UsbTransport::enumerate_devices() {
        for dev in devices {
            let mode = get_fastboot_mode_sync(&dev.serial_number);
            lines.push(format!("{}\t{}", dev.serial_number, mode));
        }
    }

    if let Ok(devices) = AdbClient::enumerate_adb_devices() {
        for dev in devices {
            let mode = get_adb_device_mode(&dev.serial);
            lines.push(format!("{}\t{}", dev.serial, mode));
        }
    }
    lines.join("\n")
}

fn get_fastboot_mode_sync(serial: &str) -> &'static str {
    let rt = unsafe { RUNTIME.as_ref() };
    if rt.is_none() {
        return "fastboot";
    }

    rt.unwrap().block_on(async {
        let transport = match UsbTransport::open(Some(serial)) {
            Ok(t) => t,
            Err(_) => return "fastboot",
        };
        let mut driver = driver::FastbootDriver::new(transport);
        match driver.get_var("is-userspace").await {
            Ok(val) if val.trim().eq_ignore_ascii_case("yes") => "fastboot (fastbootd)",
            _ => "fastboot",
        }
    })
}

fn get_adb_device_mode(serial: &str) -> &'static str {
    use adb::client::AdbClient;
    let client = match AdbClient::connect_fast(Some(serial), false) {
        Ok(c) => c,
        Err(e) => {
            if e.to_string().contains("unauthorized") {
                return "unauthorized";
            }
            return "device";
        }
    };
    let mut client = client;
    if let Ok(twrp) = client.shell("getprop ro.twrp.boot") {
        if twrp.trim() == "1" {
            return "recovery";
        }
    }
    if let Ok(mode) = client.shell("getprop ro.bootmode") {
        let mode = mode.trim().to_lowercase();
        if mode.contains("recovery") {
            return "recovery";
        }
        if mode.contains("charger") {
            return "charger";
        }
    }
    "device"
}

#[no_mangle]
pub extern "C" fn fastboot_getvar(
    serial: *const c_char,
    variable: *const c_char,
    out_buf: *mut c_char,
    buf_len: usize,
) -> i32 {
    check_init!();
    let serial = unsafe { ptr_to_option_str(serial) };
    let variable = unsafe {
        if variable.is_null() {
            return -3;
        }
        CStr::from_ptr(variable).to_str().unwrap_or("")
    };

    let rt = get_runtime!();
    let result = rt.block_on(async {
        let transport = UsbTransport::open(serial)?;
        let mut driver = driver::FastbootDriver::new(transport);
        if variable == "all" {
            let vars = driver.get_var_all().await?;
            Ok(vars.join("\n"))
        } else {
            driver.get_var(variable).await
        }
    });
    match result {
        Ok(value) => write_to_buffer(&value, out_buf, buf_len),
        Err(_) => -4,
    }
}
#[no_mangle]
pub extern "C" fn fastboot_reboot(serial: *const c_char, target: *const c_char) -> i32 {
    check_init!();
    let serial = unsafe { ptr_to_option_str(serial) };
    let target = unsafe { ptr_to_str(target) };
    get_runtime!().block_on(async { do_reboot(serial, target).await })
}

async fn do_reboot(serial: Option<&str>, target: &str) -> i32 {
    use adb::client::AdbClient;
    let target_lower = target.to_lowercase();
    let target_normalized: &str = match target_lower.as_str() {
        "" | "system" => "",
        "bl" | "bootloader" | "fastboot" => "bootloader",
        "rec" | "recovery" => "recovery",
        "fbd" | "fastbootd" | "userspace" => "fastboot",
        _ => target,
    };
    if let Ok(devices) = AdbClient::enumerate_adb_devices() {
        let has_adb = devices
            .iter()
            .any(|d| serial.map_or(true, |s| d.serial == s));
        if has_adb && !devices.is_empty() {
            if let Ok(mut client) = AdbClient::connect_with_auth(serial) {
                let mode = if target_normalized.is_empty() {
                    None
                } else {
                    Some(target_normalized)
                };
                if client.reboot(mode).is_ok() {
                    return 0;
                }
            } else if adb::client::adb_cli_reboot_proxy(
                &serial.map(|s| s.to_string()),
                Some(target_normalized),
                false,
            )
            .is_ok()
            {
                return 0;
            }
        }
    }
    if let Ok(transport) = UsbTransport::open(serial) {
        let mut driver = driver::FastbootDriver::new(transport);
        let result = if target_normalized.is_empty() {
            driver.reboot().await
        } else {
            driver.reboot_to(target_normalized).await
        };
        return if result.is_ok() { 0 } else { -4 };
    }
    -5
}
#[no_mangle]
pub extern "C" fn fastboot_erase(serial: *const c_char, partition: *const c_char) -> i32 {
    check_init!();
    let serial = unsafe { ptr_to_option_str(serial) };
    let partition = unsafe { ptr_to_str(partition) };
    if partition.is_empty() {
        return -3;
    }

    get_runtime!().block_on(async {
        let transport = match UsbTransport::open(serial) {
            Ok(t) => t,
            Err(_) => return -5,
        };
        let mut driver = driver::FastbootDriver::new(transport);
        match driver.erase(partition).await {
            Ok(_) => 0,
            Err(_) => -8,
        }
    })
}
#[no_mangle]
pub extern "C" fn fastboot_set_active(serial: *const c_char, slot: *const c_char) -> i32 {
    check_init!();
    let serial = unsafe { ptr_to_option_str(serial) };
    let slot = unsafe { ptr_to_str(slot) };
    let slot = match slot.to_lowercase().as_str() {
        "a" | "_a" | "slot_a" => "a",
        "b" | "_b" | "slot_b" => "b",
        _ => return -3,
    };

    get_runtime!().block_on(async {
        let transport = match UsbTransport::open(serial) {
            Ok(t) => t,
            Err(_) => return -5,
        };
        let mut driver = driver::FastbootDriver::new(transport);
        match driver.set_active(slot).await {
            Ok(_) => 0,
            Err(_) => -8,
        }
    })
}
#[no_mangle]
pub extern "C" fn fastboot_oem(
    serial: *const c_char,
    command: *const c_char,
    out_buf: *mut c_char,
    buf_len: usize,
) -> i32 {
    check_init!();
    let serial = unsafe { ptr_to_option_str(serial) };
    let command = unsafe { ptr_to_str(command) };
    if command.is_empty() {
        return -3;
    }

    let rt = get_runtime!();
    let result = rt.block_on(async {
        let transport = UsbTransport::open(serial)?;
        let mut driver = driver::FastbootDriver::new(transport);
        driver.oem_command(command).await
    });
    match result {
        Ok(msg) => write_to_buffer(&msg, out_buf, buf_len),
        Err(FastbootError::Device(msg)) => {
            write_to_buffer(&format!("FAILED: {}", msg), out_buf, buf_len)
        }
        Err(_) => -8,
    }
}
#[no_mangle]
pub extern "C" fn fastboot_flashing(
    serial: *const c_char,
    operation: *const c_char,
    out_buf: *mut c_char,
    buf_len: usize,
) -> i32 {
    check_init!();
    let serial = unsafe { ptr_to_option_str(serial) };
    let operation = unsafe { ptr_to_str(operation) };

    let cmd = match operation.to_lowercase().as_str() {
        "lock" => "flashing lock",
        "unlock" => "flashing unlock",
        "lock_critical" | "lock-critical" => "flashing lock_critical",
        "unlock_critical" | "unlock-critical" => "flashing unlock_critical",
        "get_unlock_ability" | "get-unlock-ability" => "flashing get_unlock_ability",
        _ => return -3,
    };

    let rt = get_runtime!();
    let result = rt.block_on(async {
        let transport = UsbTransport::open(serial)?;
        let mut driver = driver::FastbootDriver::new(transport);
        driver.raw_command(cmd).await
    });
    match result {
        Ok(protocol::Response::Okay(msg)) => write_to_buffer(&msg, out_buf, buf_len),
        Ok(protocol::Response::Fail(msg)) => {
            write_to_buffer(&format!("FAILED: {}", msg), out_buf, buf_len)
        }
        _ => -8,
    }
}
#[no_mangle]
pub extern "C" fn fastboot_format(
    serial: *const c_char,
    partition: *const c_char,
    fs_type: *const c_char,
    size: *const c_char,
) -> i32 {
    check_init!();
    let serial = unsafe { ptr_to_option_str(serial) };
    let partition = unsafe { ptr_to_str(partition) };
    let fs_type = unsafe { ptr_to_option_str(fs_type) };
    let size = unsafe { ptr_to_option_str(size) };
    if partition.is_empty() {
        return -3;
    }

    let cmd = match (fs_type, size) {
        (Some(fs), Some(sz)) => format!("format:{}:{}:{}", fs, sz, partition),
        (Some(fs), None) => format!("format:{}:{}", fs, partition),
        (None, Some(sz)) => format!("format:ext4:{}:{}", sz, partition),
        (None, None) => format!("format:{}", partition),
    };

    get_runtime!().block_on(async {
        let transport = match UsbTransport::open(serial) {
            Ok(t) => t,
            Err(_) => return -5,
        };
        let mut driver = driver::FastbootDriver::new(transport);
        match driver.raw_command(&cmd).await {
            Ok(protocol::Response::Okay(_)) => 0,
            _ => -8,
        }
    })
}
#[no_mangle]
pub extern "C" fn fastboot_boot(
    serial: *const c_char,
    kernel_path: *const c_char,
    ramdisk_path: *const c_char,
) -> i32 {
    check_init!();
    let serial = unsafe { ptr_to_option_str(serial) };
    let kernel_path = unsafe { ptr_to_str(kernel_path) };
    let ramdisk_path = unsafe { ptr_to_option_str(ramdisk_path) };

    let kernel = Path::new(kernel_path);
    if !kernel.exists() {
        return -6;
    }

    get_runtime!().block_on(async {
        let mut boot_data = match fs::read(kernel) {
            Ok(d) => d,
            Err(_) => return -7,
        };

        if let Some(rd_path) = ramdisk_path {
            let rd = Path::new(rd_path);
            if rd.exists() {
                if let Ok(rd_data) = fs::read(rd) {
                    boot_data.extend(rd_data);
                }
            }
        }

        let transport = match UsbTransport::open(serial) {
            Ok(t) => t,
            Err(_) => return -5,
        };
        let mut driver = driver::FastbootDriver::new(transport);
        if driver.download(&boot_data).await.is_err() {
            return -8;
        }
        match driver.boot().await {
            Ok(_) => 0,
            Err(_) => -8,
        }
    })
}
#[no_mangle]
pub extern "C" fn fastboot_fetch(
    serial: *const c_char,
    partition: *const c_char,
    output_path: *const c_char,
) -> i64 {
    check_init!();
    let serial = unsafe { ptr_to_option_str(serial) };
    let partition = unsafe { ptr_to_str(partition) };
    let output_path = unsafe { ptr_to_str(output_path) };
    if partition.is_empty() || output_path.is_empty() {
        return -3;
    }

    get_runtime!().block_on(async {
        let transport = match UsbTransport::open(serial) {
            Ok(t) => t,
            Err(_) => return -5,
        };
        let mut driver = driver::FastbootDriver::new(transport);
        match driver
            .read_partition(partition, Path::new(output_path))
            .await
        {
            Ok(size) => size as i64,
            Err(_) => -8,
        }
    })
}
#[no_mangle]
pub extern "C" fn fastboot_create_partition(
    serial: *const c_char,
    name: *const c_char,
    size: u64,
) -> i32 {
    check_init!();
    let serial = unsafe { ptr_to_option_str(serial) };
    let name = unsafe { ptr_to_str(name) };
    if name.is_empty() {
        return -3;
    }

    get_runtime!().block_on(async {
        let transport = match UsbTransport::open(serial) {
            Ok(t) => t,
            Err(_) => return -5,
        };
        let mut driver = driver::FastbootDriver::new(transport);
        match driver.create_partition(name, size).await {
            Ok(_) => 0,
            Err(_) => -8,
        }
    })
}
#[no_mangle]
pub extern "C" fn fastboot_delete_partition(serial: *const c_char, name: *const c_char) -> i32 {
    check_init!();
    let serial = unsafe { ptr_to_option_str(serial) };
    let name = unsafe { ptr_to_str(name) };
    if name.is_empty() {
        return -3;
    }

    get_runtime!().block_on(async {
        let transport = match UsbTransport::open(serial) {
            Ok(t) => t,
            Err(_) => return -5,
        };
        let mut driver = driver::FastbootDriver::new(transport);
        match driver.delete_partition(name).await {
            Ok(_) => 0,
            Err(_) => -8,
        }
    })
}
#[no_mangle]
pub extern "C" fn fastboot_resize_partition(
    serial: *const c_char,
    name: *const c_char,
    size: u64,
) -> i32 {
    check_init!();
    let serial = unsafe { ptr_to_option_str(serial) };
    let name = unsafe { ptr_to_str(name) };
    if name.is_empty() {
        return -3;
    }

    get_runtime!().block_on(async {
        let transport = match UsbTransport::open(serial) {
            Ok(t) => t,
            Err(_) => return -5,
        };
        let mut driver = driver::FastbootDriver::new(transport);
        match driver.resize_partition(name, size).await {
            Ok(_) => 0,
            Err(_) => -8,
        }
    })
}
#[no_mangle]
pub extern "C" fn fastboot_snapshot_update(serial: *const c_char, operation: *const c_char) -> i32 {
    check_init!();
    let serial = unsafe { ptr_to_option_str(serial) };
    let operation = unsafe { ptr_to_str(operation) };

    let op = match operation.to_lowercase().as_str() {
        "cancel" => "cancel",
        "merge" => "merge",
        _ => return -3,
    };

    get_runtime!().block_on(async {
        let transport = match UsbTransport::open(serial) {
            Ok(t) => t,
            Err(_) => return -5,
        };
        let mut driver = driver::FastbootDriver::new(transport);
        let cmd = format!("snapshot-update:{}", op);
        match driver.raw_command(&cmd).await {
            Ok(protocol::Response::Okay(_)) => 0,
            _ => -8,
        }
    })
}
#[no_mangle]
pub extern "C" fn fastboot_gsi(
    serial: *const c_char,
    operation: *const c_char,
    out_buf: *mut c_char,
    buf_len: usize,
) -> i32 {
    check_init!();
    let serial = unsafe { ptr_to_option_str(serial) };
    let operation = unsafe { ptr_to_str(operation) };

    let op = match operation.to_lowercase().as_str() {
        "wipe" => "wipe",
        "disable" => "disable",
        "status" => "status",
        _ => return -3,
    };

    let rt = get_runtime!();
    let result = rt.block_on(async {
        let transport = UsbTransport::open(serial)?;
        let mut driver = driver::FastbootDriver::new(transport);
        let cmd = format!("gsi:{}", op);
        driver.raw_command(&cmd).await
    });
    match result {
        Ok(protocol::Response::Okay(msg)) => write_to_buffer(&msg, out_buf, buf_len),
        Ok(protocol::Response::Fail(msg)) => {
            write_to_buffer(&format!("FAILED: {}", msg), out_buf, buf_len)
        }
        _ => -8,
    }
}
#[no_mangle]
pub extern "C" fn fastboot_wipe_super(
    serial: *const c_char,
    super_empty_path: *const c_char,
) -> i32 {
    check_init!();
    let serial = unsafe { ptr_to_option_str(serial) };
    let super_empty_path = unsafe { ptr_to_option_str(super_empty_path) };

    get_runtime!().block_on(async {
        let transport = match UsbTransport::open(serial) {
            Ok(t) => t,
            Err(_) => return -5,
        };
        let mut driver = driver::FastbootDriver::new(transport);

        if let Some(path) = super_empty_path {
            let p = Path::new(path);
            if p.exists() {
                if let Ok(data) = fs::read(p) {
                    if driver.download(&data).await.is_err() {
                        return -8;
                    }
                }
            }
        }

        match driver.raw_command("wipe-super").await {
            Ok(protocol::Response::Okay(_)) => 0,
            _ => -8,
        }
    })
}
#[no_mangle]
pub extern "C" fn fastboot_stage(serial: *const c_char, input_path: *const c_char) -> i32 {
    check_init!();
    let serial = unsafe { ptr_to_option_str(serial) };
    let input_path = unsafe { ptr_to_str(input_path) };
    let path = Path::new(input_path);
    if !path.exists() {
        return -6;
    }

    get_runtime!().block_on(async {
        let data = match fs::read(path) {
            Ok(d) => d,
            Err(_) => return -7,
        };
        let transport = match UsbTransport::open(serial) {
            Ok(t) => t,
            Err(_) => return -5,
        };
        let mut driver = driver::FastbootDriver::new(transport);
        match driver.download(&data).await {
            Ok(_) => 0,
            Err(_) => -8,
        }
    })
}
#[no_mangle]
pub extern "C" fn fastboot_get_staged(serial: *const c_char, output_path: *const c_char) -> i64 {
    check_init!();
    let serial = unsafe { ptr_to_option_str(serial) };
    let output_path = unsafe { ptr_to_str(output_path) };
    if output_path.is_empty() {
        return -3;
    }

    get_runtime!().block_on(async {
        let transport = match UsbTransport::open(serial) {
            Ok(t) => t,
            Err(_) => return -5,
        };
        let mut driver = driver::FastbootDriver::new(transport);
        match driver.upload().await {
            Ok(data) => {
                if fs::write(output_path, &data).is_err() {
                    return -7;
                }
                data.len() as i64
            }
            Err(_) => -8,
        }
    })
}

#[no_mangle]
pub extern "C" fn fastboot_flash(
    serial: *const c_char,
    partition: *const c_char,
    image_path: *const c_char,
    callback: Option<ProgressCallback>,
) -> i32 {
    check_init!();
    let serial = unsafe { ptr_to_option_str(serial) };
    let partition = unsafe { ptr_to_str(partition) };
    let image_path = unsafe { ptr_to_str(image_path) };
    if partition.is_empty() || image_path.is_empty() {
        return -3;
    }

    get_runtime!().block_on(async { do_flash(serial, partition, image_path, callback).await })
}

async fn do_flash(
    serial: Option<&str>,
    partition: &str,
    image_path: &str,
    callback: Option<ProgressCallback>,
) -> i32 {
    let path = Path::new(image_path);
    if !path.exists() {
        return -6;
    }

    let file_size = match fs::metadata(path) {
        Ok(m) => m.len(),
        Err(_) => return -7,
    };
    let transport = match UsbTransport::open(serial) {
        Ok(t) => t,
        Err(_) => return -5,
    };
    let mut driver = driver::FastbootDriver::new(transport);

    let max_download = driver
        .get_max_download_size()
        .await
        .unwrap_or(512 * 1024 * 1024);
    let is_sparse = sparse::is_sparse_file(path).unwrap_or(false);

    if let Some(cb) = callback {
        let status = CString::new(format!("Sending '{}'", partition)).unwrap();
        driver.set_progress_callback(Box::new(move |current, total| {
            cb(current, total, status.as_ptr());
        }));
    }

    let result = if file_size > max_download {
        if is_sparse {
            flash_sparse_chunked_lib(&mut driver, partition, path, max_download, callback).await
        } else {
            flash_raw_chunked_lib(&mut driver, partition, path, max_download, callback).await
        }
    } else {
        flash_single_lib(&mut driver, partition, path, file_size, callback).await
    };

    match result {
        Ok(_) => 0,
        Err(_) => -8,
    }
}

async fn flash_single_lib(
    driver: &mut driver::FastbootDriver<UsbTransport>,
    partition: &str,
    path: &Path,
    file_size: u64,
    callback: Option<ProgressCallback>,
) -> Result<(), FastbootError> {
    let data = fs::read(path).map_err(FastbootError::Io)?;

    if let Some(cb) = callback {
        let status = CString::new(format!("Sending '{}'", partition)).unwrap();
        driver.set_progress_callback(Box::new(move |current, total| {
            cb(current, total, status.as_ptr());
        }));
    }

    driver.download(&data).await?;

    if let Some(cb) = callback {
        let status = CString::new(format!("Writing '{}'", partition)).unwrap();
        cb(file_size, file_size, status.as_ptr());
    }

    driver.flash(partition).await?;
    Ok(())
}

async fn flash_sparse_chunked_lib(
    driver: &mut driver::FastbootDriver<UsbTransport>,
    partition: &str,
    path: &Path,
    max_download: u64,
    callback: Option<ProgressCallback>,
) -> Result<(), FastbootError> {
    use sparse::StreamingResparse;

    let mut resparse = StreamingResparse::new(path, max_download)
        .map_err(|e| FastbootError::InvalidArg(format!("解析 sparse 文件失败: {}", e)))?;

    let total_size = resparse.total_transfer_size();
    let mut sent: u64 = 0;

    driver.set_timeout(Duration::from_secs(300));

    while let Some((fragment_data, _idx, _is_last)) = resparse
        .next_fragment()
        .map_err(|e| FastbootError::InvalidArg(format!("生成 sparse fragment 失败: {}", e)))?
    {
        let chunk_size = fragment_data.len() as u64;

        if let Some(cb) = callback {
            let status = CString::new(format!("Sending '{}'", partition)).unwrap();
            cb(sent, total_size, status.as_ptr());
        }

        driver.download(&fragment_data).await?;
        driver.flash(partition).await?;
        sent += chunk_size;
    }

    driver.set_timeout(Duration::from_secs(30));
    Ok(())
}

async fn flash_raw_chunked_lib(
    driver: &mut driver::FastbootDriver<UsbTransport>,
    partition: &str,
    path: &Path,
    max_download: u64,
    callback: Option<ProgressCallback>,
) -> Result<(), FastbootError> {
    use memmap2::Mmap;
    use sparse::{CHUNK_HEADER_SIZE, SPARSE_HEADER_MAGIC, SPARSE_HEADER_SIZE};
    use std::fs::File;

    let file = File::open(path).map_err(FastbootError::Io)?;
    let file_size = file.metadata().map_err(FastbootError::Io)?.len();
    let mmap = unsafe { Mmap::map(&file).map_err(FastbootError::Io)? };

    let block_size = 4096u32;
    let max_overhead = SPARSE_HEADER_SIZE + 3 * CHUNK_HEADER_SIZE;
    let max_data = ((max_download - max_overhead as u64) / block_size as u64) * block_size as u64;

    let num_chunks = ((file_size + max_data - 1) / max_data) as usize;
    let total_blocks = ((file_size + block_size as u64 - 1) / block_size as u64) as u32;
    let mut sent: u64 = 0;

    driver.set_timeout(Duration::from_secs(300));

    for i in 0..num_chunks {
        let offset = i as u64 * max_data;
        let chunk_data_size = std::cmp::min(max_data, file_size - offset);
        let chunk_blocks = ((chunk_data_size + block_size as u64 - 1) / block_size as u64) as u32;
        let start_block = (offset / block_size as u64) as u32;
        let end_block = start_block + chunk_blocks;
        let trailing = total_blocks.saturating_sub(end_block);
        let aligned = (chunk_blocks as usize) * (block_size as usize);

        let mut num_sparse_chunks = 1u32;
        if start_block > 0 {
            num_sparse_chunks += 1;
        }
        if trailing > 0 {
            num_sparse_chunks += 1;
        }

        let mut buf = Vec::with_capacity(max_overhead + aligned);
        buf.extend_from_slice(&SPARSE_HEADER_MAGIC.to_le_bytes());
        buf.extend_from_slice(&1u16.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&(SPARSE_HEADER_SIZE as u16).to_le_bytes());
        buf.extend_from_slice(&(CHUNK_HEADER_SIZE as u16).to_le_bytes());
        buf.extend_from_slice(&block_size.to_le_bytes());
        buf.extend_from_slice(&total_blocks.to_le_bytes());
        buf.extend_from_slice(&num_sparse_chunks.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes());

        if start_block > 0 {
            buf.extend_from_slice(&0xCAC3u16.to_le_bytes());
            buf.extend_from_slice(&0u16.to_le_bytes());
            buf.extend_from_slice(&start_block.to_le_bytes());
            buf.extend_from_slice(&(CHUNK_HEADER_SIZE as u32).to_le_bytes());
        }

        let raw_total = (CHUNK_HEADER_SIZE + aligned) as u32;
        buf.extend_from_slice(&0xCAC1u16.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&chunk_blocks.to_le_bytes());
        buf.extend_from_slice(&raw_total.to_le_bytes());
        buf.extend_from_slice(&mmap[offset as usize..(offset + chunk_data_size) as usize]);

        if (chunk_data_size as usize) < aligned {
            buf.extend(std::iter::repeat(0u8).take(aligned - chunk_data_size as usize));
        }

        if trailing > 0 {
            buf.extend_from_slice(&0xCAC3u16.to_le_bytes());
            buf.extend_from_slice(&0u16.to_le_bytes());
            buf.extend_from_slice(&trailing.to_le_bytes());
            buf.extend_from_slice(&(CHUNK_HEADER_SIZE as u32).to_le_bytes());
        }

        if let Some(cb) = callback {
            let status = CString::new(format!("Sending '{}'", partition)).unwrap();
            cb(sent, file_size, status.as_ptr());
        }

        driver.download(&buf).await?;
        driver.flash(partition).await?;
        sent += chunk_data_size;
    }

    driver.set_timeout(Duration::from_secs(30));
    Ok(())
}

#[no_mangle]
pub extern "C" fn adb_shell(
    serial: *const c_char,
    command: *const c_char,
    out_buf: *mut c_char,
    buf_len: usize,
) -> i32 {
    check_init!();
    let serial = unsafe { ptr_to_option_str(serial) };
    let command = unsafe { ptr_to_str(command) };

    use adb::client::AdbClient;
    let mut client = match AdbClient::connect_with_auth(serial) {
        Ok(c) => c,
        Err(_) => return -5,
    };
    match client.shell(command) {
        Ok(output) => write_to_buffer(&output, out_buf, buf_len),
        Err(_) => -8,
    }
}
#[no_mangle]
pub extern "C" fn adb_push(
    serial: *const c_char,
    local_path: *const c_char,
    remote_path: *const c_char,
) -> i32 {
    check_init!();
    let serial = unsafe { ptr_to_option_str(serial) };
    let local_path = unsafe { ptr_to_str(local_path) };
    let remote_path = unsafe { ptr_to_str(remote_path) };

    let local = Path::new(local_path);
    if !local.exists() {
        return -6;
    }

    use adb::client::AdbClient;
    let mut client = match AdbClient::connect_with_auth(serial) {
        Ok(c) => c,
        Err(_) => return -5,
    };
    match client.push(Path::new(local), Path::new(remote_path)) {
        Ok(_) => 0,
        Err(_) => -8,
    }
}
#[no_mangle]
pub extern "C" fn adb_pull(
    serial: *const c_char,
    remote_path: *const c_char,
    local_path: *const c_char,
) -> i64 {
    check_init!();
    let serial = unsafe { ptr_to_option_str(serial) };
    let remote_path = unsafe { ptr_to_str(remote_path) };
    let local_path = unsafe { ptr_to_str(local_path) };

    use adb::client::AdbClient;
    let mut client = match AdbClient::connect_with_auth(serial) {
        Ok(c) => c,
        Err(_) => return -5,
    };
    match client.pull(Path::new(remote_path), Path::new(local_path)) {
        Ok(size) => size as i64,
        Err(_) => -8,
    }
}
#[no_mangle]
pub extern "C" fn adb_install(
    serial: *const c_char,
    apk_path: *const c_char,
    replace: i32,
    out_buf: *mut c_char,
    buf_len: usize,
) -> i32 {
    check_init!();
    let serial = unsafe { ptr_to_option_str(serial) };
    let apk_path = unsafe { ptr_to_str(apk_path) };
    let apk = Path::new(apk_path);
    if !apk.exists() {
        return -6;
    }

    use adb::client::AdbClient;
    let mut client = match AdbClient::connect_with_auth(serial) {
        Ok(c) => c,
        Err(_) => return -5,
    };

    let remote_path = format!(
        "/data/local/tmp/{}",
        apk.file_name().unwrap().to_string_lossy()
    );
    if client.push(apk, Path::new(&remote_path)).is_err() {
        return -8;
    }

    let install_cmd = if replace != 0 {
        format!("pm install -r {}", remote_path)
    } else {
        format!("pm install {}", remote_path)
    };

    let output = match client.shell(&install_cmd) {
        Ok(o) => o,
        Err(_) => return -8,
    };
    let _ = client.shell(&format!("rm {}", remote_path));

    write_to_buffer(&output, out_buf, buf_len)
}
#[no_mangle]
pub extern "C" fn adb_uninstall(
    serial: *const c_char,
    package: *const c_char,
    out_buf: *mut c_char,
    buf_len: usize,
) -> i32 {
    check_init!();
    let serial = unsafe { ptr_to_option_str(serial) };
    let package = unsafe { ptr_to_str(package) };
    if package.is_empty() {
        return -3;
    }

    use adb::client::AdbClient;
    let mut client = match AdbClient::connect_with_auth(serial) {
        Ok(c) => c,
        Err(_) => return -5,
    };
    match client.shell(&format!("pm uninstall {}", package)) {
        Ok(output) => write_to_buffer(&output, out_buf, buf_len),
        Err(_) => -8,
    }
}
#[no_mangle]
pub extern "C" fn adb_packages(
    serial: *const c_char,
    third_party: i32,
    system: i32,
    out_buf: *mut c_char,
    buf_len: usize,
) -> i32 {
    check_init!();
    let serial = unsafe { ptr_to_option_str(serial) };

    let cmd = if third_party != 0 {
        "pm list packages -3"
    } else if system != 0 {
        "pm list packages -s"
    } else {
        "pm list packages"
    };

    use adb::client::AdbClient;
    let mut client = match AdbClient::connect_with_auth(serial) {
        Ok(c) => c,
        Err(_) => return -5,
    };
    match client.shell(cmd) {
        Ok(output) => {
            let cleaned: String = output
                .lines()
                .filter_map(|l| l.strip_prefix("package:"))
                .collect::<Vec<_>>()
                .join("\n");
            write_to_buffer(&cleaned, out_buf, buf_len)
        }
        Err(_) => -8,
    }
}
#[no_mangle]
pub extern "C" fn adb_logcat(
    serial: *const c_char,
    filter: *const c_char,
    out_buf: *mut c_char,
    buf_len: usize,
) -> i32 {
    check_init!();
    let serial = unsafe { ptr_to_option_str(serial) };
    let filter = unsafe { ptr_to_str(filter) };

    let cmd = if filter.is_empty() {
        "logcat -d".to_string()
    } else {
        format!("logcat -d {}", filter)
    };

    use adb::client::AdbClient;
    let mut client = match AdbClient::connect_with_auth(serial) {
        Ok(c) => c,
        Err(_) => return -5,
    };
    match client.shell(&cmd) {
        Ok(output) => write_to_buffer(&output, out_buf, buf_len),
        Err(_) => -8,
    }
}
#[no_mangle]
pub extern "C" fn adb_screencap(serial: *const c_char, output_path: *const c_char) -> i32 {
    check_init!();
    let serial = unsafe { ptr_to_option_str(serial) };
    let output_path = unsafe { ptr_to_str(output_path) };
    if output_path.is_empty() {
        return -3;
    }

    use adb::client::AdbClient;
    let mut client = match AdbClient::connect_with_auth(serial) {
        Ok(c) => c,
        Err(_) => return -5,
    };

    let remote_path = "/data/local/tmp/screenshot.png";
    if client
        .shell(&format!("screencap -p {}", remote_path))
        .is_err()
    {
        return -8;
    }
    if client
        .pull(Path::new(remote_path), Path::new(output_path))
        .is_err()
    {
        return -8;
    }
    let _ = client.shell(&format!("rm {}", remote_path));
    0
}
#[no_mangle]
pub extern "C" fn adb_screenrecord(
    serial: *const c_char,
    output_path: *const c_char,
    time_limit: u32,
) -> i32 {
    check_init!();
    let serial = unsafe { ptr_to_option_str(serial) };
    let output_path = unsafe { ptr_to_str(output_path) };
    if output_path.is_empty() {
        return -3;
    }

    use adb::client::AdbClient;
    let mut client = match AdbClient::connect_with_auth(serial) {
        Ok(c) => c,
        Err(_) => return -5,
    };

    let remote_path = "/data/local/tmp/recording.mp4";
    let _ = client.shell(&format!(
        "screenrecord --time-limit {} {}",
        time_limit, remote_path
    ));
    if client
        .pull(Path::new(remote_path), Path::new(output_path))
        .is_err()
    {
        return -8;
    }
    let _ = client.shell(&format!("rm {}", remote_path));
    0
}
#[no_mangle]
pub extern "C" fn adb_reboot(serial: *const c_char, target: *const c_char) -> i32 {
    check_init!();
    let serial = unsafe { ptr_to_option_str(serial) };
    let target = unsafe { ptr_to_str(target) };

    use adb::client::AdbClient;
    let mut client = match AdbClient::connect_with_auth(serial) {
        Ok(c) => c,
        Err(_) => return -5,
    };

    let mode = if target.is_empty() {
        None
    } else {
        Some(target)
    };
    match client.reboot(mode) {
        Ok(_) => 0,
        Err(_) => -8,
    }
}

pub type TaskCallback = extern "C" fn(
    task_index: u32,
    total_tasks: u32,
    action: u32,
    partition: *const c_char,
    image_path: *const c_char,
);

#[no_mangle]
pub extern "C" fn fastboot_flashall(
    serial: *const c_char,
    directory: *const c_char,
    wipe: i32,
    task_callback: Option<TaskCallback>,
    progress_callback: Option<ProgressCallback>,
) -> i32 {
    check_init!();
    let serial = unsafe { ptr_to_option_str(serial) };
    let directory = unsafe { ptr_to_option_str(directory) };

    get_runtime!().block_on(async {
        do_flashall(
            serial,
            directory,
            wipe != 0,
            task_callback,
            progress_callback,
        )
        .await
    })
}

async fn do_flashall(
    serial: Option<&str>,
    directory: Option<&str>,
    wipe: bool,
    task_callback: Option<TaskCallback>,
    progress_callback: Option<ProgressCallback>,
) -> i32 {
    use std::env;
    let original_dir = env::current_dir().ok();
    if let Some(dir) = directory {
        if env::set_current_dir(dir).is_err() {
            return -6;
        }
    }
    let flash_script = match detect_flash_package_lib() {
        Ok(s) => s,
        Err(_) => {
            if let Some(ref orig) = original_dir {
                let _ = env::set_current_dir(orig);
            }
            return -3;
        }
    };

    if flash_script.tasks.is_empty() {
        if let Some(ref orig) = original_dir {
            let _ = env::set_current_dir(orig);
        }
        return -3;
    }
    let transport = match UsbTransport::open(serial) {
        Ok(t) => t,
        Err(_) => {
            if let Some(ref orig) = original_dir {
                let _ = env::set_current_dir(orig);
            }
            return -5;
        }
    };

    let mut driver = driver::FastbootDriver::new(transport);
    let max_download = driver
        .get_max_download_size()
        .await
        .unwrap_or(512 * 1024 * 1024);

    let total_tasks = flash_script.tasks.len() as u32;
    for (i, task) in flash_script.tasks.iter().enumerate() {
        match &task.action {
            FlashActionLib::Erase(partition) => {
                if let Some(cb) = task_callback {
                    let part_cstr = CString::new(partition.as_str()).unwrap();
                    cb(i as u32, total_tasks, 0, part_cstr.as_ptr(), ptr::null());
                }
                if driver.erase(partition).await.is_err() {
                    if let Some(ref orig) = original_dir {
                        let _ = env::set_current_dir(orig);
                    }
                    return -8;
                }
            }
            FlashActionLib::Flash(partition, image_path) => {
                if let Some(cb) = task_callback {
                    let part_cstr = CString::new(partition.as_str()).unwrap();
                    let path_cstr = CString::new(image_path.to_string_lossy().as_ref()).unwrap();
                    cb(
                        i as u32,
                        total_tasks,
                        1,
                        part_cstr.as_ptr(),
                        path_cstr.as_ptr(),
                    );
                }

                let file_size = match fs::metadata(image_path) {
                    Ok(m) => m.len(),
                    Err(_) => continue,
                };

                let is_sparse = sparse::is_sparse_file(image_path).unwrap_or(false);

                let result = if file_size > max_download {
                    if is_sparse {
                        flash_sparse_chunked_lib(
                            &mut driver,
                            partition,
                            image_path,
                            max_download,
                            progress_callback,
                        )
                        .await
                    } else {
                        flash_raw_chunked_lib(
                            &mut driver,
                            partition,
                            image_path,
                            max_download,
                            progress_callback,
                        )
                        .await
                    }
                } else {
                    flash_single_lib(
                        &mut driver,
                        partition,
                        image_path,
                        file_size,
                        progress_callback,
                    )
                    .await
                };

                if result.is_err() {
                    if let Some(ref orig) = original_dir {
                        let _ = env::set_current_dir(orig);
                    }
                    return -8;
                }
            }
            FlashActionLib::SetActive(slot) => {
                if let Some(cb) = task_callback {
                    let slot_cstr = CString::new(slot.as_str()).unwrap();
                    cb(i as u32, total_tasks, 2, slot_cstr.as_ptr(), ptr::null());
                }
                if driver.set_active(slot).await.is_err() {}
            }
            FlashActionLib::Reboot => {
                if let Some(cb) = task_callback {
                    cb(i as u32, total_tasks, 3, ptr::null(), ptr::null());
                }
                let _ = driver.reboot().await;
            }
        }
    }
    if wipe {
        if let Some(cb) = task_callback {
            let part_cstr = CString::new("userdata").unwrap();
            cb(
                total_tasks,
                total_tasks + 1,
                0,
                part_cstr.as_ptr(),
                ptr::null(),
            );
        }
        let _ = driver.erase("userdata").await;
    }
    if let Some(ref orig) = original_dir {
        let _ = env::set_current_dir(orig);
    }

    0
}
#[derive(Debug, Clone)]
enum FlashActionLib {
    Erase(String),
    Flash(String, std::path::PathBuf),
    SetActive(String),
    Reboot,
}

struct FlashTaskLib {
    action: FlashActionLib,
}

struct FlashScriptLib {
    tasks: Vec<FlashTaskLib>,
}

fn detect_flash_package_lib() -> Result<FlashScriptLib, FastbootError> {
    let current_dir = std::env::current_dir().map_err(FastbootError::Io)?;
    let flash_all_bat = current_dir.join("flash_all.bat");
    let flash_all_sh = current_dir.join("flash_all.sh");

    if flash_all_bat.exists() {
        return parse_flash_all_bat_lib(&flash_all_bat);
    }
    if flash_all_sh.exists() {
        return parse_flash_all_sh_lib(&flash_all_sh);
    }
    let images_dir = current_dir.join("images");
    if images_dir.exists() && images_dir.is_dir() {
        return scan_xiaomi_package_lib(&images_dir);
    }
    scan_standard_images_lib(&current_dir)
}

fn parse_flash_all_bat_lib(path: &Path) -> Result<FlashScriptLib, FastbootError> {
    let content = fs::read_to_string(path).map_err(FastbootError::Io)?;
    let mut tasks = Vec::new();
    let base_dir = path.parent().unwrap_or(Path::new("."));

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty()
            || line.starts_with("REM")
            || line.starts_with("@")
            || line.starts_with("::")
        {
            continue;
        }
        if let Some(cmd) = extract_fastboot_command_lib(line) {
            if let Some(task) = parse_fastboot_command_lib(&cmd, base_dir) {
                tasks.push(task);
            }
        }
    }
    Ok(FlashScriptLib { tasks })
}

fn parse_flash_all_sh_lib(path: &Path) -> Result<FlashScriptLib, FastbootError> {
    let content = fs::read_to_string(path).map_err(FastbootError::Io)?;
    let mut tasks = Vec::new();
    let base_dir = path.parent().unwrap_or(Path::new("."));

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("#") {
            continue;
        }
        if let Some(cmd) = extract_fastboot_command_lib(line) {
            if let Some(task) = parse_fastboot_command_lib(&cmd, base_dir) {
                tasks.push(task);
            }
        }
    }
    Ok(FlashScriptLib { tasks })
}

fn extract_fastboot_command_lib(line: &str) -> Option<String> {
    let line = line
        .replace("%FASTBOOT%", "fastboot")
        .replace("$FASTBOOT", "fastboot")
        .replace("${FASTBOOT}", "fastboot")
        .replace("fastboot %*", "fastboot")
        .replace("fastboot %* ", "fastboot ");

    if let Some(pos) = line.to_lowercase().find("fastboot") {
        let cmd = &line[pos..];
        let cmd = cmd.split("||").next().unwrap_or(cmd);
        let cmd = cmd.split("&&").next().unwrap_or(cmd);
        let cmd = cmd.split("2>&1").next().unwrap_or(cmd);
        let cmd = cmd.split("2>").next().unwrap_or(cmd);
        return Some(cmd.trim().to_string());
    }
    None
}

fn parse_fastboot_command_lib(cmd: &str, base_dir: &Path) -> Option<FlashTaskLib> {
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    if parts.len() < 2 {
        return None;
    }

    let cmd_parts = if parts[0].to_lowercase().contains("fastboot") {
        &parts[1..]
    } else {
        &parts[..]
    };
    if cmd_parts.is_empty() {
        return None;
    }

    match cmd_parts[0].to_lowercase().as_str() {
        "flash" if cmd_parts.len() >= 3 => {
            let partition = cmd_parts[1].to_string();
            if partition == "crclist" || partition == "sparsecrclist" {
                return None;
            }
            let image_path = resolve_image_path_lib(cmd_parts[2], base_dir);
            if image_path.exists() {
                Some(FlashTaskLib {
                    action: FlashActionLib::Flash(partition, image_path),
                })
            } else {
                None
            }
        }
        "erase" if cmd_parts.len() >= 2 => Some(FlashTaskLib {
            action: FlashActionLib::Erase(cmd_parts[1].to_string()),
        }),
        "set_active" | "set-active" if cmd_parts.len() >= 2 => Some(FlashTaskLib {
            action: FlashActionLib::SetActive(cmd_parts[1].to_string()),
        }),
        "reboot" => Some(FlashTaskLib {
            action: FlashActionLib::Reboot,
        }),
        _ => None,
    }
}

fn resolve_image_path_lib(path_str: &str, base_dir: &Path) -> std::path::PathBuf {
    let path_str = path_str
        .replace("%~dp0", "")
        .replace("$PWD/", "")
        .replace("./", "")
        .replace("\\", "/");
    base_dir.join(path_str)
}

fn scan_xiaomi_package_lib(images_dir: &Path) -> Result<FlashScriptLib, FastbootError> {
    let mut tasks = Vec::new();
    let partitions = [
        ("xbl_ab", "xbl.elf"),
        ("xbl_config_ab", "xbl_config.elf"),
        ("abl_ab", "abl.elf"),
        ("tz_ab", "tz.mbn"),
        ("hyp_ab", "hyp.mbn"),
        ("devcfg_ab", "devcfg.mbn"),
        ("storsec", "storsec.mbn"),
        ("bluetooth_ab", "BTFM.bin"),
        ("cmnlib_ab", "cmnlib.mbn"),
        ("cmnlib64_ab", "cmnlib64.mbn"),
        ("modem_ab", "NON-HLOS.bin"),
        ("dsp_ab", "dspso.bin"),
        ("keymaster_ab", "km41.mbn"),
        ("logo", "logo.img"),
        ("super", "super.img"),
        ("vbmeta_ab", "vbmeta.img"),
        ("dtbo_ab", "dtbo.img"),
        ("boot_ab", "boot.img"),
    ];

    for (partition, filename) in &partitions {
        let image_path = images_dir.join(filename);
        if image_path.exists() {
            tasks.push(FlashTaskLib {
                action: FlashActionLib::Flash(partition.to_string(), image_path),
            });
        }
    }

    tasks.push(FlashTaskLib {
        action: FlashActionLib::SetActive("a".to_string()),
    });
    Ok(FlashScriptLib { tasks })
}

fn scan_standard_images_lib(dir: &Path) -> Result<FlashScriptLib, FastbootError> {
    let mut tasks = Vec::new();
    let images = partition::get_standard_images();

    for img in &images {
        let image_path = dir.join(&img.img_name);
        if image_path.exists() {
            tasks.push(FlashTaskLib {
                action: FlashActionLib::Flash(img.part_name.clone(), image_path),
            });
        }
    }
    Ok(FlashScriptLib { tasks })
}

#[no_mangle]
pub extern "C" fn fastboot_version() -> *const c_char {
    static VERSION: &[u8] = b"fastboot-rs 0.1.0\0";
    VERSION.as_ptr() as *const c_char
}
#[no_mangle]
pub extern "C" fn fastboot_error_string(code: i32) -> *const c_char {
    static ERRORS: &[&[u8]] = &[
        b"Success\0",
        b"Invalid token\0",
        b"Buffer too small\0",
        b"Invalid parameter\0",
        b"Device error\0",
        b"No device found\0",
        b"File not found\0",
        b"File read error\0",
        b"Operation failed\0",
    ];

    let idx = (-code) as usize;
    if idx < ERRORS.len() {
        ERRORS[idx].as_ptr() as *const c_char
    } else {
        b"Unknown error\0".as_ptr() as *const c_char
    }
}
#[no_mangle]
pub extern "C" fn fastboot_get_token() -> u64 {
    VALID_TOKEN
}
