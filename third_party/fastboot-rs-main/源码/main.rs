
mod cli;
mod driver;
mod error;
mod flash;
mod partition;
mod progress;
mod protocol;
mod sparse;
mod tcp_transport;
mod transport;
mod udp_transport;
mod usb_transport;
mod util;
mod adb;

use std::fs;
use std::path::Path;

use clap::Parser;

use cli::{Cli, Commands};
use error::FastbootError;
use flash::ImageSource;
use progress::{format_size, format_speed, print_error, print_info, print_success, set_machine_readable, FlashProgress, ChunkedProgress, Spinner, SimpleProgressBar};
use usb_transport::UsbTransport;

#[tokio::main]
async fn main() {
    env_logger::init();

    let cli = Cli::parse();

    if let Err(e) = run(cli).await {
        print_error(&e.to_string());

        if let Some(hint) = e.recovery_hint() {
            eprintln!("\n{}", hint);
        }

        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<(), FastbootError> {
    if cli.json {
        progress::set_machine_readable(true);
    }

    match cli.command {
        Commands::Devices => {
            cmd_devices().await?;
        }
        Commands::Getvar { variable } => {
            cmd_getvar(&cli.serial, &variable, cli.verbose).await?;
        }
        Commands::Flash { partition, filename } => {
            cmd_flash(&cli.serial, &partition, &filename, cli.verbose).await?;
        }
        Commands::Erase { partition } => {
            cmd_erase(&cli.serial, &partition).await?;
        }
        Commands::Reboot { target } => {
            cmd_reboot(&cli.serial, &target).await?;
        }
        Commands::Flashall { wipe } => {
            cmd_flashall(&cli.serial, wipe).await?;
        }
        Commands::Update { filename } => {
            cmd_update(&cli.serial, &filename).await?;
        }
        Commands::Upload { partition, filename } => {
            cmd_upload(&cli.serial, &partition, &filename).await?;
        }
        Commands::Diagnose => {
            cmd_diagnose().await?;
        }
        Commands::Shell { command } => {
            cmd_adb_shell(&cli.serial, &command, cli.verbose).await?;
        }
        Commands::Push { local, remote } => {
            cmd_adb_push(&cli.serial, &local, &remote, cli.verbose).await?;
        }
        Commands::Pull { remote, local } => {
            cmd_adb_pull(&cli.serial, &remote, &local, cli.verbose).await?;
        }
        Commands::Install { apk, replace } => {
            cmd_adb_install(&cli.serial, &apk, replace).await?;
        }
        Commands::Uninstall { package } => {
            cmd_adb_uninstall(&cli.serial, &package).await?;
        }
        Commands::Packages { third_party, system } => {
            cmd_adb_packages(&cli.serial, third_party, system).await?;
        }
        Commands::Logcat { filter } => {
            cmd_adb_logcat(&cli.serial, &filter).await?;
        }
        Commands::Screencap { output } => {
            cmd_adb_screencap(&cli.serial, &output).await?;
        }
        Commands::Screenrecord { output, time } => {
            cmd_adb_screenrecord(&cli.serial, &output, time).await?;
        }
        Commands::SetActive { slot } => {
            cmd_set_active(&cli.serial, &slot).await?;
        }
        Commands::Oem { command } => {
            cmd_oem(&cli.serial, &command).await?;
        }
        Commands::Flashing { operation } => {
            cmd_flashing(&cli.serial, &operation).await?;
        }
        Commands::Format { partition, fs_type, size } => {
            cmd_format(&cli.serial, &partition, fs_type.as_deref(), size.as_deref()).await?;
        }
        Commands::Boot { kernel, ramdisk } => {
            cmd_boot(&cli.serial, &kernel, ramdisk.as_deref()).await?;
        }
        Commands::Fetch { partition, output } => {
            cmd_fetch(&cli.serial, &partition, &output).await?;
        }
        Commands::CreateLogicalPartition { name, size } => {
            cmd_create_logical_partition(&cli.serial, &name, size).await?;
        }
        Commands::DeleteLogicalPartition { name } => {
            cmd_delete_logical_partition(&cli.serial, &name).await?;
        }
        Commands::ResizeLogicalPartition { name, size } => {
            cmd_resize_logical_partition(&cli.serial, &name, size).await?;
        }
        Commands::SnapshotUpdate { operation } => {
            cmd_snapshot_update(&cli.serial, &operation).await?;
        }
        Commands::Gsi { operation } => {
            cmd_gsi(&cli.serial, &operation).await?;
        }
        Commands::WipeSuper { super_empty } => {
            cmd_wipe_super(&cli.serial, super_empty.as_deref()).await?;
        }
        Commands::Stage { input } => {
            cmd_stage(&cli.serial, &input).await?;
        }
        Commands::GetStaged { output } => {
            cmd_get_staged(&cli.serial, &output).await?;
        }
    }

    Ok(())
}

async fn cmd_devices() -> Result<(), FastbootError> {
    use adb::client::AdbClient;

    let mut found_any = false;

    let fastboot_devices = UsbTransport::enumerate_devices()
        .map_err(FastbootError::Transport)?;

    let adb_devices = AdbClient::enumerate_adb_devices()
        .unwrap_or_default();

    if fastboot_devices.is_empty() && adb_devices.is_empty() {
        println!("No devices found");
        return Ok(());
    }

    println!("List of devices attached");

    for dev in &fastboot_devices {
        let mode = get_fastboot_mode(&dev.serial_number).await;
        println!("{}\t{}", dev.serial_number, mode);
        found_any = true;
    }

    for dev in &adb_devices {
        let mode = get_adb_device_mode(&dev.serial);
        println!("{}\t{}", dev.serial, mode);
        found_any = true;
    }

    if !found_any {
        println!("No devices found");
    }

    Ok(())
}

async fn get_fastboot_mode(serial: &str) -> &'static str {
    let transport = match UsbTransport::open(Some(serial)) {
        Ok(t) => t,
        Err(_) => return "fastboot",
    };

    let mut driver = driver::FastbootDriver::new(transport);

    match driver.get_var("is-userspace").await {
        Ok(val) if val.trim().eq_ignore_ascii_case("yes") => "fastboot (fastbootd)",
        _ => "fastboot",
    }
}

fn get_adb_device_mode(serial: &str) -> &'static str {
    use adb::client::AdbClient;

    let client = match AdbClient::connect_fast(Some(serial), false) {
        Ok(c) => c,
        Err(e) => {
            let err_msg = e.to_string();
            if err_msg.contains("unauthorized") {
                return "unauthorized";
            }
            return "device";
        }
    };

    let mut client = client;

    if let Ok(twrp_boot) = client.shell("getprop ro.twrp.boot") {
        if twrp_boot.trim() == "1" {
            return "recovery";
        }
    }

    if let Ok(bootmode) = client.shell("getprop ro.bootmode") {
        let bootmode = bootmode.trim().to_lowercase();
        if bootmode.contains("recovery") {
            return "recovery";
        }
        if bootmode.contains("charger") {
            return "charger";
        }
    }

    if let Ok(usb_state) = client.shell("getprop sys.usb.state") {
        if usb_state.trim().contains("sideload") {
            return "sideload";
        }
    }

    "device"
}

async fn cmd_getvar(serial: &Option<String>, variable: &str, verbose: bool) -> Result<(), FastbootError> {
    let transport = open_transport(serial).await?;
    let mut driver = driver::FastbootDriver::new(transport);

    if verbose {
        print_info(&format!("连接到设备: {}", serial.as_deref().unwrap_or("(auto)")));
    }

    if variable == "all" {
        let vars = driver.get_var_all().await?;
        for var in vars {
            println!("{}", var);
        }
    } else {
        let value = driver.get_var(variable).await?;
        println!("{}: {}", variable, value);
    }

    Ok(())
}

async fn cmd_flash(
    serial: &Option<String>,
    partition: &str,
    filename: &Path,
    verbose: bool,
) -> Result<(), FastbootError> {
    if !filename.exists() {
        return Err(FastbootError::ImageNotFound(filename.display().to_string()));
    }

    let file_size = fs::metadata(filename)
        .map_err(FastbootError::Io)?
        .len();

    if file_size == 0 {
        return Err(FastbootError::InvalidArg("镜像文件为空".to_string()));
    }

    let transport = open_transport(serial).await?;
    let mut driver = driver::FastbootDriver::new(transport);

    let max_download_size = driver.get_max_download_size().await.unwrap_or(512 * 1024 * 1024);

    if verbose {
        print_info(&format!("文件: {}", filename.display()));
        print_info(&format!("大小: {}", format_size(file_size)));
        print_info(&format!("设备限制: {}", format_size(max_download_size)));
    }

    let is_sparse = sparse::is_sparse_file(filename).unwrap_or(false);

    if file_size > max_download_size {
        if is_sparse {
            println!("Flashing '{}' (sparse, {})...", partition, format_size(file_size));
            flash_sparse_chunked(&mut driver, partition, filename, max_download_size).await?;
        } else {
            println!("Flashing '{}' (raw, {})...", partition, format_size(file_size));
            flash_raw_chunked(&mut driver, partition, filename, max_download_size).await?;
        }
    } else {
        flash_single(&mut driver, partition, filename, file_size).await?;
    }

    Ok(())
}

async fn flash_single(
    driver: &mut driver::FastbootDriver<UsbTransport>,
    partition: &str,
    filename: &Path,
    file_size: u64,
) -> Result<(), FastbootError> {
    use progress::FlashProgress;
    use std::time::Duration;

    let is_json = progress::is_machine_readable();

    if is_json {
        println!(r#"{{"type":"start","partition":"{}","size":{}}}"#, partition, file_size);
    }

    let progress = FlashProgress::new(partition, file_size, "Sending");

    let data = fs::read(filename).map_err(FastbootError::Io)?;

    if is_json {
        println!(r#"{{"type":"sending","partition":"{}","size":{}}}"#, partition, file_size);
    }

    driver.set_progress_callback(progress::make_bar_callback(progress.inner().clone()));

    let timeout_secs = std::cmp::max(60, (file_size / (50 * 1024 * 1024) + 1) * 30) as u64;
    driver.set_timeout(Duration::from_secs(timeout_secs));

    driver.download(&data).await?;

    drop(data);

    progress.set_operation("Writing");
    if is_json {
        println!(r#"{{"type":"writing","partition":"{}"}}"#, partition);
    }

    driver.flash(partition).await?;

    progress.finish();
    Ok(())
}

async fn flash_sparse_chunked(
    driver: &mut driver::FastbootDriver<UsbTransport>,
    partition: &str,
    filename: &Path,
    _max_download_size: u64,
) -> Result<(), FastbootError> {
    use sparse::{StreamingResparse, validate_sparse_file};
    use progress::ChunkedProgress;
    use std::time::Duration;

    let is_json = progress::is_machine_readable();
    let file_size = fs::metadata(filename).map_err(FastbootError::Io)?.len();

    if !is_json {
        print!("Validating sparse image... ");
        std::io::Write::flush(&mut std::io::stdout()).ok();
    }

    let validation = validate_sparse_file(filename);
    if !validation.valid {
        let error_msg = validation.error.unwrap_or_else(|| "未知错误".to_string());
        return Err(FastbootError::InvalidArg(format!(
            "Sparse 文件校验失败: {}\n文件: {}\n大小: {} 字节",
            error_msg, filename.display(), file_size
        )));
    }

    if !is_json {
        println!("OK ({})", progress::format_size(file_size));
    }

    let max_download_size = driver.get_var("max-download-size").await
        .ok()
        .and_then(|s| parse_hex_or_dec(&s))
        .unwrap_or(512 * 1024 * 1024);

    let use_prefetch = get_available_memory_gb() >= 10.0;

    if !is_json {
        print!("Parsing sparse image... ");
        std::io::Write::flush(&mut std::io::stdout()).ok();
    }

    let mut resparse = StreamingResparse::new(filename, max_download_size)
        .map_err(|e| FastbootError::InvalidArg(format!("解析 sparse 文件失败: {}", e)))?;

    let total_fragments = resparse.total_fragments();
    let total_size = resparse.total_transfer_size();

    if !is_json {
        println!("{} chunks", total_fragments);
    }

    driver.set_timeout(Duration::from_secs(300));

    let mut chunked_progress = ChunkedProgress::new(partition, total_size, total_fragments);

    if use_prefetch {
        flash_sparse_with_prefetch_sync(driver, &mut resparse, &mut chunked_progress, partition, total_fragments).await?;
    } else {
        flash_sparse_serial_sync(driver, &mut resparse, &mut chunked_progress, partition).await?;
    }

    drop(resparse);

    chunked_progress.finish();

    driver.set_timeout(Duration::from_secs(30));

    Ok(())
}

fn parse_hex_or_dec(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.starts_with("0x") || s.starts_with("0X") {
        u64::from_str_radix(&s[2..], 16).ok()
    } else {
        s.parse().ok()
    }
}

fn get_available_memory_gb() -> f64 {
    #[cfg(target_os = "windows")]
    {
        use std::mem::MaybeUninit;

        #[repr(C)]
        struct MEMORYSTATUSEX {
            dw_length: u32,
            dw_memory_load: u32,
            ull_total_phys: u64,
            ull_avail_phys: u64,
            ull_total_page_file: u64,
            ull_avail_page_file: u64,
            ull_total_virtual: u64,
            ull_avail_virtual: u64,
            ull_avail_extended_virtual: u64,
        }

        #[link(name = "kernel32")]
        extern "system" {
            fn GlobalMemoryStatusEx(lpBuffer: *mut MEMORYSTATUSEX) -> i32;
        }

        unsafe {
            let mut mem_info = MaybeUninit::<MEMORYSTATUSEX>::uninit();
            (*mem_info.as_mut_ptr()).dw_length = std::mem::size_of::<MEMORYSTATUSEX>() as u32;

            if GlobalMemoryStatusEx(mem_info.as_mut_ptr()) != 0 {
                let mem_info = mem_info.assume_init();
                return mem_info.ull_avail_phys as f64 / (1024.0 * 1024.0 * 1024.0);
            }
        }
        16.0
    }

    #[cfg(not(target_os = "windows"))]
    {
        16.0
    }
}

async fn flash_sparse_with_prefetch_sync(
    driver: &mut driver::FastbootDriver<UsbTransport>,
    resparse: &mut sparse::StreamingResparse,
    chunked_progress: &mut progress::ChunkedProgress,
    partition: &str,
    total_fragments: usize,
) -> Result<(), FastbootError> {
    use std::time::Duration;

    let mut current_data: Option<Vec<u8>> = resparse.next_fragment()
        .map_err(|e| FastbootError::InvalidArg(format!("生成 sparse fragment 失败: {}", e)))?
        .map(|(data, _, _)| data);

    let mut fragment_idx = 0usize;

    let file_path = resparse.file_path().to_path_buf();
    let fragment_ranges = resparse.fragment_ranges_clone();
    let header = *resparse.header();
    let chunk_metas = resparse.chunk_metas_clone();

    while let Some(fragment_data) = current_data.take() {
        let chunk_size = fragment_data.len() as u64;
        let is_last = fragment_idx + 1 >= total_fragments;

        let prefetch_task = if !is_last {
            let next_idx = fragment_idx + 1;
            let file_path = file_path.clone();
            let fragment_ranges = fragment_ranges.clone();
            let chunk_metas = chunk_metas.clone();

            Some(tokio::task::spawn_blocking(move || {
                sparse::build_fragment_standalone(
                    &file_path,
                    &header,
                    &chunk_metas,
                    &fragment_ranges[next_idx],
                )
            }))
        } else {
            None
        };

        let timeout_secs = std::cmp::max(300, (chunk_size / (50 * 1024 * 1024) + 1) * 30) as u64;
        driver.set_timeout(Duration::from_secs(timeout_secs));

        let chunk_progress = chunked_progress.start_chunk(chunk_size);

        driver.set_progress_callback(progress::make_bar_callback(chunk_progress.inner().clone()));

        driver.download(&fragment_data).await?;

        drop(fragment_data);

        chunk_progress.set_operation("Writing");

        driver.flash(partition).await?;

        chunk_progress.finish();
        chunked_progress.finish_chunk(chunk_size);

        if let Some(task) = prefetch_task {
            current_data = Some(task.await
                .map_err(|e| FastbootError::InvalidArg(format!("预读失败: {}", e)))?
                .map_err(|e| FastbootError::InvalidArg(format!("生成 sparse fragment 失败: {}", e)))?);
        }

        fragment_idx += 1;
    }

    Ok(())
}

async fn flash_sparse_serial_sync(
    driver: &mut driver::FastbootDriver<UsbTransport>,
    resparse: &mut sparse::StreamingResparse,
    chunked_progress: &mut progress::ChunkedProgress,
    partition: &str,
) -> Result<(), FastbootError> {
    use std::time::Duration;

    while let Some((fragment_data, _idx, _is_last)) = resparse.next_fragment()
        .map_err(|e| FastbootError::InvalidArg(format!("生成 sparse fragment 失败: {}", e)))?
    {
        let chunk_size = fragment_data.len() as u64;

        let timeout_secs = std::cmp::max(300, (chunk_size / (50 * 1024 * 1024) + 1) * 30) as u64;
        driver.set_timeout(Duration::from_secs(timeout_secs));

        let chunk_progress = chunked_progress.start_chunk(chunk_size);

        driver.set_progress_callback(progress::make_bar_callback(chunk_progress.inner().clone()));

        driver.download(&fragment_data).await?;

        drop(fragment_data);

        chunk_progress.set_operation("Writing");

        driver.flash(partition).await?;

        chunk_progress.finish();
        chunked_progress.finish_chunk(chunk_size);
    }

    Ok(())
}

async fn flash_raw_chunked(
    driver: &mut driver::FastbootDriver<UsbTransport>,
    partition: &str,
    filename: &Path,
    _max_download_size: u64,
) -> Result<(), FastbootError> {
    use sparse::{SPARSE_HEADER_MAGIC, SPARSE_HEADER_SIZE, CHUNK_HEADER_SIZE};
    use std::io::{Read, Seek, SeekFrom};
    use std::time::Duration;
    use progress::ChunkedProgress;

    let is_json = progress::is_machine_readable();

    let mut file = fs::File::open(filename).map_err(FastbootError::Io)?;
    let file_size = file.metadata().map_err(FastbootError::Io)?.len();

    if file_size > 100 * 1024 * 1024 {
        if !is_json {
            print!("Verifying connection... ");
            std::io::Write::flush(&mut std::io::stdout()).ok();
        }

        match driver.get_var("version").await {
            Ok(_) => {
                if !is_json {
                    println!("OK");
                }
            }
            Err(e) => {
                if !is_json {
                    println!("WARN ({})", e);
                }
                let _ = driver.reset().await;
            }
        }

        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    let max_download_size = driver.get_var("max-download-size").await
        .ok()
        .and_then(|s| parse_hex_or_dec(&s))
        .unwrap_or(512 * 1024 * 1024);

    let _ = driver.reset().await;

    let block_size = 4096u32;

    let max_overhead = SPARSE_HEADER_SIZE + 3 * CHUNK_HEADER_SIZE;
    let max_data_per_chunk = ((max_download_size - max_overhead as u64) / block_size as u64) * block_size as u64;

    let num_chunks = ((file_size + max_data_per_chunk - 1) / max_data_per_chunk) as usize;
    let total_image_blocks = ((file_size + block_size as u64 - 1) / block_size as u64) as u32;

    let mut chunked_progress = ChunkedProgress::new(partition, file_size, num_chunks);

    let buffer_capacity = max_overhead + max_data_per_chunk as usize;
    let mut buffer = Vec::with_capacity(buffer_capacity);

    let mut offset = 0u64;

    for chunk_index in 0..num_chunks {
        let remaining = file_size - offset;
        let chunk_data_size = std::cmp::min(remaining, max_data_per_chunk);
        let chunk_blocks = ((chunk_data_size + block_size as u64 - 1) / block_size as u64) as u32;
        let start_block = (offset / block_size as u64) as u32;
        let end_block = start_block + chunk_blocks;
        let trailing_blocks = total_image_blocks.saturating_sub(end_block);
        let aligned_size = (chunk_blocks as usize) * (block_size as usize);

        let mut num_sparse_chunks = 1u32;
        if start_block > 0 {
            num_sparse_chunks += 1;
        }
        if trailing_blocks > 0 {
            num_sparse_chunks += 1;
        }

        buffer.clear();

        buffer.extend_from_slice(&SPARSE_HEADER_MAGIC.to_le_bytes());
        buffer.extend_from_slice(&1u16.to_le_bytes());
        buffer.extend_from_slice(&0u16.to_le_bytes());
        buffer.extend_from_slice(&(SPARSE_HEADER_SIZE as u16).to_le_bytes());
        buffer.extend_from_slice(&(CHUNK_HEADER_SIZE as u16).to_le_bytes());
        buffer.extend_from_slice(&block_size.to_le_bytes());
        buffer.extend_from_slice(&total_image_blocks.to_le_bytes());
        buffer.extend_from_slice(&num_sparse_chunks.to_le_bytes());
        buffer.extend_from_slice(&0u32.to_le_bytes());

        if start_block > 0 {
            buffer.extend_from_slice(&0xCAC3u16.to_le_bytes());
            buffer.extend_from_slice(&0u16.to_le_bytes());
            buffer.extend_from_slice(&start_block.to_le_bytes());
            buffer.extend_from_slice(&(CHUNK_HEADER_SIZE as u32).to_le_bytes());
        }

        let raw_chunk_total_sz = (CHUNK_HEADER_SIZE + aligned_size) as u32;
        buffer.extend_from_slice(&0xCAC1u16.to_le_bytes());
        buffer.extend_from_slice(&0u16.to_le_bytes());
        buffer.extend_from_slice(&chunk_blocks.to_le_bytes());
        buffer.extend_from_slice(&raw_chunk_total_sz.to_le_bytes());

        file.seek(SeekFrom::Start(offset)).map_err(FastbootError::Io)?;
        let data_start = buffer.len();
        buffer.resize(data_start + chunk_data_size as usize, 0);
        file.read_exact(&mut buffer[data_start..]).map_err(FastbootError::Io)?;

        if (chunk_data_size as usize) < aligned_size {
            let padding = aligned_size - chunk_data_size as usize;
            buffer.extend(std::iter::repeat(0u8).take(padding));
        }

        if trailing_blocks > 0 {
            buffer.extend_from_slice(&0xCAC3u16.to_le_bytes());
            buffer.extend_from_slice(&0u16.to_le_bytes());
            buffer.extend_from_slice(&trailing_blocks.to_le_bytes());
            buffer.extend_from_slice(&(CHUNK_HEADER_SIZE as u32).to_le_bytes());
        }

        let chunk_progress = chunked_progress.start_chunk(chunk_data_size);

        driver.set_progress_callback(progress::make_bar_callback(chunk_progress.inner().clone()));

        let timeout_secs = std::cmp::max(120, (chunk_data_size / (100 * 1024 * 1024) + 1) * 60) as u64;
        driver.set_timeout(Duration::from_secs(timeout_secs));

        driver.download(&buffer).await?;

        buffer.clear();
        buffer.shrink_to(buffer_capacity);

        chunk_progress.set_operation("Writing");

        driver.flash(partition).await?;

        driver.set_timeout(Duration::from_secs(30));

        chunk_progress.finish();
        chunked_progress.finish_chunk(chunk_data_size);

        offset += chunk_data_size;
    }

    drop(buffer);
    drop(file);

    chunked_progress.finish();

    driver.set_timeout(Duration::from_secs(30));

    Ok(())
}

async fn cmd_erase(serial: &Option<String>, partition: &str) -> Result<(), FastbootError> {
    let transport = open_transport(serial).await?;
    let mut driver = driver::FastbootDriver::new(transport);

    let mut spinner = Spinner::new(&format!("擦除 '{}'...", partition));

    for _ in 0..10 {
        spinner.tick();
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }

    driver.erase(partition).await?;
    spinner.finish(&format!("擦除 {} 完成", partition));

    Ok(())
}

async fn cmd_set_active(serial: &Option<String>, slot: &str) -> Result<(), FastbootError> {
    let transport = open_transport(serial).await?;
    let mut driver = driver::FastbootDriver::new(transport);

    let slot = slot.to_lowercase();
    let slot = match slot.as_str() {
        "a" | "_a" | "slot_a" => "a",
        "b" | "_b" | "slot_b" => "b",
        _ => return Err(FastbootError::InvalidArg(format!("无效的槽位: {}，必须是 a 或 b", slot))),
    };

    print_info(&format!("设置活动槽位为 {}...", slot));
    driver.set_active(slot).await?;
    print_success(&format!("活动槽位已设置为 {}", slot));

    Ok(())
}

async fn cmd_oem(serial: &Option<String>, command: &[String]) -> Result<(), FastbootError> {
    let transport = open_transport(serial).await?;
    let mut driver = driver::FastbootDriver::new(transport);

    let cmd = command.join(" ");
    print_info(&format!("执行 OEM 命令: {}", cmd));

    match driver.oem_command(&cmd).await {
        Ok(msg) => {
            if !msg.is_empty() {
                println!("{}", msg);
            }
            print_success("OEM 命令执行成功");
        }
        Err(FastbootError::Device(msg)) => {
            println!("FAILED: {}", msg);
        }
        Err(e) => return Err(e),
    }

    Ok(())
}

async fn cmd_flashing(serial: &Option<String>, operation: &str) -> Result<(), FastbootError> {
    let transport = open_transport(serial).await?;
    let mut driver = driver::FastbootDriver::new(transport);

    let cmd = match operation.to_lowercase().as_str() {
        "lock" => "flashing lock",
        "unlock" => "flashing unlock",
        "lock_critical" | "lock-critical" => "flashing lock_critical",
        "unlock_critical" | "unlock-critical" => "flashing unlock_critical",
        "get_unlock_ability" | "get-unlock-ability" => "flashing get_unlock_ability",
        _ => return Err(FastbootError::InvalidArg(format!(
            "无效的操作: {}，支持: lock, unlock, lock_critical, unlock_critical, get_unlock_ability",
            operation
        ))),
    };

    print_info(&format!("执行: {}", cmd));

    let response = driver.raw_command(cmd).await?;
    match response {
        protocol::Response::Okay(msg) => {
            if !msg.is_empty() {
                println!("{}", msg);
            }
            print_success("操作成功");
        }
        protocol::Response::Fail(msg) => {
            println!("FAILED: {}", msg);
        }
        _ => {}
    }

    Ok(())
}

async fn cmd_format(
    serial: &Option<String>,
    partition: &str,
    fs_type: Option<&str>,
    size: Option<&str>,
) -> Result<(), FastbootError> {
    let transport = open_transport(serial).await?;
    let mut driver = driver::FastbootDriver::new(transport);

    let cmd = match (fs_type, size) {
        (Some(fs), Some(sz)) => format!("format:{}:{}:{}", fs, sz, partition),
        (Some(fs), None) => format!("format:{}:{}", fs, partition),
        (None, Some(sz)) => format!("format:ext4:{}:{}", sz, partition),
        (None, None) => format!("format:{}", partition),
    };

    print_info(&format!("格式化分区 '{}'...", partition));

    let response = driver.raw_command(&cmd).await?;
    match response {
        protocol::Response::Okay(_) => {
            print_success(&format!("分区 '{}' 格式化完成", partition));
        }
        protocol::Response::Fail(msg) => {
            return Err(FastbootError::Device(msg));
        }
        _ => {}
    }

    Ok(())
}

async fn cmd_boot(
    serial: &Option<String>,
    kernel: &Path,
    ramdisk: Option<&Path>,
) -> Result<(), FastbootError> {
    if !kernel.exists() {
        return Err(FastbootError::ImageNotFound(kernel.display().to_string()));
    }

    let transport = open_transport(serial).await?;
    let mut driver = driver::FastbootDriver::new(transport);

    let mut boot_data = fs::read(kernel).map_err(FastbootError::Io)?;
    print_info(&format!("内核: {} ({})", kernel.display(), format_size(boot_data.len() as u64)));

    if let Some(rd) = ramdisk {
        if !rd.exists() {
            return Err(FastbootError::ImageNotFound(rd.display().to_string()));
        }
        let rd_data = fs::read(rd).map_err(FastbootError::Io)?;
        print_info(&format!("Ramdisk: {} ({})", rd.display(), format_size(rd_data.len() as u64)));
        boot_data.extend(rd_data);
    }

    print_info("下载启动镜像...");
    driver.download(&boot_data).await?;

    print_info("启动...");
    driver.boot().await?;

    print_success("启动命令已发送");
    Ok(())
}

async fn cmd_fetch(
    serial: &Option<String>,
    partition: &str,
    output: &Path,
) -> Result<(), FastbootError> {
    let transport = open_transport(serial).await?;
    let mut driver = driver::FastbootDriver::new(transport);

    print_info(&format!("从设备读取分区 '{}'...", partition));

    let size = driver.read_partition(partition, output).await?;

    print_success(&format!("已保存到 {} ({})", output.display(), format_size(size)));
    Ok(())
}

async fn cmd_create_logical_partition(
    serial: &Option<String>,
    name: &str,
    size: u64,
) -> Result<(), FastbootError> {
    let transport = open_transport(serial).await?;
    let mut driver = driver::FastbootDriver::new(transport);

    print_info(&format!("创建逻辑分区 '{}' (大小: {})...", name, format_size(size)));
    driver.create_partition(name, size).await?;
    print_success(&format!("逻辑分区 '{}' 创建成功", name));

    Ok(())
}

async fn cmd_delete_logical_partition(
    serial: &Option<String>,
    name: &str,
) -> Result<(), FastbootError> {
    let transport = open_transport(serial).await?;
    let mut driver = driver::FastbootDriver::new(transport);

    print_info(&format!("删除逻辑分区 '{}'...", name));
    driver.delete_partition(name).await?;
    print_success(&format!("逻辑分区 '{}' 已删除", name));

    Ok(())
}

async fn cmd_resize_logical_partition(
    serial: &Option<String>,
    name: &str,
    size: u64,
) -> Result<(), FastbootError> {
    let transport = open_transport(serial).await?;
    let mut driver = driver::FastbootDriver::new(transport);

    print_info(&format!("调整逻辑分区 '{}' 大小为 {}...", name, format_size(size)));
    driver.resize_partition(name, size).await?;
    print_success(&format!("逻辑分区 '{}' 大小已调整", name));

    Ok(())
}

async fn cmd_snapshot_update(
    serial: &Option<String>,
    operation: &str,
) -> Result<(), FastbootError> {
    let transport = open_transport(serial).await?;
    let mut driver = driver::FastbootDriver::new(transport);

    let op = match operation.to_lowercase().as_str() {
        "cancel" => "cancel",
        "merge" => "merge",
        _ => return Err(FastbootError::InvalidArg(format!(
            "无效的操作: {}，支持: cancel, merge",
            operation
        ))),
    };

    let cmd = format!("snapshot-update:{}", op);
    print_info(&format!("执行快照更新操作: {}", op));

    let response = driver.raw_command(&cmd).await?;
    match response {
        protocol::Response::Okay(msg) => {
            if !msg.is_empty() {
                println!("{}", msg);
            }
            print_success("操作成功");
        }
        protocol::Response::Fail(msg) => {
            return Err(FastbootError::Device(msg));
        }
        _ => {}
    }

    Ok(())
}

async fn cmd_gsi(serial: &Option<String>, operation: &str) -> Result<(), FastbootError> {
    let transport = open_transport(serial).await?;
    let mut driver = driver::FastbootDriver::new(transport);

    let op = match operation.to_lowercase().as_str() {
        "wipe" => "wipe",
        "disable" => "disable",
        "status" => "status",
        _ => return Err(FastbootError::InvalidArg(format!(
            "无效的操作: {}，支持: wipe, disable, status",
            operation
        ))),
    };

    let cmd = format!("gsi:{}", op);
    print_info(&format!("执行 GSI 操作: {}", op));

    let response = driver.raw_command(&cmd).await?;
    match response {
        protocol::Response::Okay(msg) => {
            if !msg.is_empty() {
                println!("{}", msg);
            }
            print_success("操作成功");
        }
        protocol::Response::Fail(msg) => {
            return Err(FastbootError::Device(msg));
        }
        _ => {}
    }

    Ok(())
}

async fn cmd_wipe_super(
    serial: &Option<String>,
    super_empty: Option<&Path>,
) -> Result<(), FastbootError> {
    let transport = open_transport(serial).await?;
    let mut driver = driver::FastbootDriver::new(transport);

    if let Some(path) = super_empty {
        if !path.exists() {
            return Err(FastbootError::ImageNotFound(path.display().to_string()));
        }
        let data = fs::read(path).map_err(FastbootError::Io)?;
        print_info(&format!("下载 super_empty.img ({})...", format_size(data.len() as u64)));
        driver.download(&data).await?;
    }

    print_info("清空 super 分区...");
    let response = driver.raw_command("wipe-super").await?;
    match response {
        protocol::Response::Okay(_) => {
            print_success("super 分区已清空");
        }
        protocol::Response::Fail(msg) => {
            return Err(FastbootError::Device(msg));
        }
        _ => {}
    }

    Ok(())
}

async fn cmd_stage(serial: &Option<String>, input: &Path) -> Result<(), FastbootError> {
    if !input.exists() {
        return Err(FastbootError::ImageNotFound(input.display().to_string()));
    }

    let transport = open_transport(serial).await?;
    let mut driver = driver::FastbootDriver::new(transport);

    let data = fs::read(input).map_err(FastbootError::Io)?;
    print_info(&format!("发送 {} 到 stage ({})...", input.display(), format_size(data.len() as u64)));

    driver.download(&data).await?;
    print_success("数据已发送到 stage");

    Ok(())
}

async fn cmd_get_staged(serial: &Option<String>, output: &Path) -> Result<(), FastbootError> {
    let transport = open_transport(serial).await?;
    let mut driver = driver::FastbootDriver::new(transport);

    print_info(&format!("从 stage 获取数据到 {}...", output.display()));

    let data = driver.upload().await?;
    fs::write(output, &data).map_err(FastbootError::Io)?;

    print_success(&format!("已保存 {} 字节到 {}", data.len(), output.display()));
    Ok(())
}

async fn cmd_reboot(serial: &Option<String>, target: &str) -> Result<(), FastbootError> {
    use adb::client::AdbClient;

    let target_lower = target.to_lowercase();
    let target = match target_lower.as_str() {
        "" | "system" => "",
        "bl" | "bootloader" | "fastboot" => "bootloader",
        "rec" | "recovery" => "recovery",
        "sideload" | "sideload-auto-reboot" => "sideload",
        "fbd" | "fastbootd" | "userspace" => "fastboot",
        _ => target,
    };

    let target_serial = serial.as_deref();

    let adb_devices = AdbClient::enumerate_adb_devices().unwrap_or_default();
    let has_adb = adb_devices.iter().any(|d| {
        target_serial.map_or(true, |s| d.serial == s)
    });

    if has_adb && !adb_devices.is_empty() {
        let mut client = AdbClient::connect_with_auth(target_serial)
            .map_err(|e| FastbootError::Adb(e.to_string()))?;

        let mode = if target.is_empty() { None } else { Some(target) };
        client.reboot(mode)
            .map_err(|e| FastbootError::Adb(e.to_string()))?;

        if target.is_empty() {
            println!("Rebooting...");
        } else {
            let display_name = if target == "fastboot" { "fastbootd" } else { target };
            println!("Rebooting to {}...", display_name);
        }
        return Ok(());
    }

    let fastboot_devices = UsbTransport::enumerate_devices()
        .map_err(FastbootError::Transport)?;

    let has_fastboot = fastboot_devices.iter().any(|d| {
        target_serial.map_or(true, |s| d.serial_number == s)
    });

    if has_fastboot && !fastboot_devices.is_empty() {
        let transport = open_transport(serial).await?;
        let mut driver = driver::FastbootDriver::new(transport);

        if target.is_empty() {
            println!("Rebooting...");
            driver.reboot().await?;
        } else {
            let display_name = if target == "fastboot" { "fastbootd" } else { target };
            println!("Rebooting to {}...", display_name);
            driver.reboot_to(target).await?;
        }
        return Ok(());
    }

    Err(FastbootError::NoDevice)
}

async fn cmd_flashall(serial: &Option<String>, wipe: bool) -> Result<(), FastbootError> {
    use std::time::Instant;

    print_info("扫描刷机包...");

    let flash_script = detect_flash_package()?;

    print_info(&format!("检测到 {} 个刷写任务", flash_script.tasks.len()));

    if flash_script.tasks.is_empty() {
        return Err(FastbootError::InvalidArg(
            "未找到可刷写的镜像文件".to_string()
        ));
    }

    let transport = open_transport(serial).await?;
    let mut driver = driver::FastbootDriver::new(transport);

    let product = driver.get_var("product").await.unwrap_or_default();
    print_info(&format!("设备: {}", product));

    let max_download_size = driver.get_max_download_size().await.unwrap_or(512 * 1024 * 1024);
    print_info(&format!("最大下载大小: {}", format_size(max_download_size)));

    let total_tasks = flash_script.tasks.len();
    let start_time = Instant::now();

    for (i, task) in flash_script.tasks.iter().enumerate() {
        match &task.action {
            FlashAction::Erase(partition) => {
                println!("\n[{}/{}] 擦除 {}...", i + 1, total_tasks, partition);
                driver.erase(partition).await?;
            }
            FlashAction::Flash(partition, image_path) => {
                let file_size = fs::metadata(image_path)
                    .map_err(FastbootError::Io)?
                    .len();

                println!("\n[{}/{}] 刷写 {} -> {} ({})",
                    i + 1, total_tasks, image_path.display(), partition, format_size(file_size));

                let is_sparse = sparse::is_sparse_file(image_path).unwrap_or(false);

                if file_size > max_download_size {
                    if is_sparse {
                        flash_sparse_chunked(&mut driver, partition, image_path, max_download_size).await?;
                    } else {
                        flash_raw_chunked(&mut driver, partition, image_path, max_download_size).await?;
                    }
                } else {
                    flash_single(&mut driver, partition, image_path, file_size).await?;
                }
            }
            FlashAction::SetActive(slot) => {
                println!("\n[{}/{}] 设置活动槽位: {}", i + 1, total_tasks, slot);
                driver.set_active(slot).await?;
            }
            FlashAction::Reboot => {
                println!("\n[{}/{}] 重启设备...", i + 1, total_tasks);
                driver.reboot().await?;
            }
        }
    }

    if wipe {
        println!("\n擦除 userdata...");
        driver.erase("userdata").await?;
        print_success("userdata 已擦除");
    }

    let elapsed = start_time.elapsed();
    print_success(&format!("\n刷机完成！总耗时: {:.1}s", elapsed.as_secs_f64()));

    Ok(())
}

#[derive(Debug, Clone)]
enum FlashAction {
    Erase(String),
    Flash(String, std::path::PathBuf),
    SetActive(String),
    Reboot,
}

#[derive(Debug)]
struct FlashScript {
    tasks: Vec<FlashTask2>,
}

#[derive(Debug)]
struct FlashTask2 {
    action: FlashAction,
}

fn detect_flash_package() -> Result<FlashScript, FastbootError> {
    let current_dir = std::env::current_dir().map_err(FastbootError::Io)?;

    let flash_all_bat = current_dir.join("flash_all.bat");
    let flash_all_sh = current_dir.join("flash_all.sh");

    if flash_all_bat.exists() {
        return parse_flash_all_bat(&flash_all_bat);
    }

    if flash_all_sh.exists() {
        return parse_flash_all_sh(&flash_all_sh);
    }

    let images_dir = current_dir.join("images");
    if images_dir.exists() && images_dir.is_dir() {
        return scan_xiaomi_package(&images_dir);
    }

    scan_standard_images(&current_dir)
}

fn parse_flash_all_bat(path: &Path) -> Result<FlashScript, FastbootError> {
    let content = fs::read_to_string(path).map_err(FastbootError::Io)?;
    let mut tasks = Vec::new();

    let base_dir = path.parent().unwrap_or(Path::new("."));

    for line in content.lines() {
        let line = line.trim();

        if line.is_empty() || line.starts_with("REM") || line.starts_with("@") || line.starts_with("::") {
            continue;
        }

        if let Some(cmd) = extract_fastboot_command(line) {
            if let Some(task) = parse_fastboot_command(&cmd, base_dir) {
                tasks.push(task);
            }
        }
    }

    Ok(FlashScript { tasks })
}

fn parse_flash_all_sh(path: &Path) -> Result<FlashScript, FastbootError> {
    let content = fs::read_to_string(path).map_err(FastbootError::Io)?;
    let mut tasks = Vec::new();

    let base_dir = path.parent().unwrap_or(Path::new("."));

    for line in content.lines() {
        let line = line.trim();

        if line.is_empty() || line.starts_with("#") {
            continue;
        }

        if let Some(cmd) = extract_fastboot_command(line) {
            if let Some(task) = parse_fastboot_command(&cmd, base_dir) {
                tasks.push(task);
            }
        }
    }

    Ok(FlashScript { tasks })
}

fn extract_fastboot_command(line: &str) -> Option<String> {
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

fn parse_fastboot_command(cmd: &str, base_dir: &Path) -> Option<FlashTask2> {
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

            let image_path = resolve_image_path(cmd_parts[2], base_dir);

            if image_path.exists() {
                Some(FlashTask2 {
                    action: FlashAction::Flash(partition, image_path),
                })
            } else {
                None
            }
        }
        "erase" if cmd_parts.len() >= 2 => {
            let partition = cmd_parts[1].to_string();
            Some(FlashTask2 {
                action: FlashAction::Erase(partition),
            })
        }
        "set_active" | "set-active" if cmd_parts.len() >= 2 => {
            let slot = cmd_parts[1].to_string();
            Some(FlashTask2 {
                action: FlashAction::SetActive(slot),
            })
        }
        "reboot" => {
            Some(FlashTask2 {
                action: FlashAction::Reboot,
            })
        }
        _ => None,
    }
}

fn resolve_image_path(path_str: &str, base_dir: &Path) -> std::path::PathBuf {
    let path_str = path_str
        .replace("%~dp0", "")
        .replace("$PWD/", "")
        .replace("./", "");

    let path_str = path_str.replace("\\", "/");

    base_dir.join(path_str)
}

fn scan_xiaomi_package(images_dir: &Path) -> Result<FlashScript, FastbootError> {
    let mut tasks = Vec::new();

    tasks.push(FlashTask2 {
        action: FlashAction::Erase("boot_ab".to_string()),
    });

    let xiaomi_partitions = [
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
        ("featenabler_ab", "featenabler.mbn"),
        ("aop_ab", "aop.mbn"),
        ("qupfw_ab", "qupv3fw.elf"),
        ("uefisecapp_ab", "uefi_sec.mbn"),
        ("multiimgoem_ab", "multi_image.mbn"),
        ("super", "super.img"),
        ("misc", "misc.img"),
        ("vbmeta_ab", "vbmeta.img"),
        ("dtbo_ab", "dtbo.img"),
        ("vbmeta_system_ab", "vbmeta_system.img"),
    ];

    for (partition, filename) in &xiaomi_partitions {
        let image_path = images_dir.join(filename);
        if image_path.exists() {
            tasks.push(FlashTask2 {
                action: FlashAction::Flash(partition.to_string(), image_path),
            });
        }
    }

    tasks.push(FlashTask2 {
        action: FlashAction::Erase("metadata".to_string()),
    });

    let remaining_partitions = [
        ("userdata", "userdata.img"),
        ("cust", "cust.img"),
    ];

    for (partition, filename) in &remaining_partitions {
        let image_path = images_dir.join(filename);
        if image_path.exists() {
            tasks.push(FlashTask2 {
                action: FlashAction::Flash(partition.to_string(), image_path),
            });
        }
    }

    tasks.push(FlashTask2 {
        action: FlashAction::Erase("imagefv_ab".to_string()),
    });

    let final_partitions = [
        ("imagefv_ab", "imagefv.elf"),
        ("rescue", "rescue.img"),
        ("spunvm", "spunvm.bin"),
        ("vendor_boot_ab", "vendor_boot.img"),
        ("logfs", "logfs_ufs_8mb.bin"),
        ("boot_ab", "boot.img"),
    ];

    for (partition, filename) in &final_partitions {
        let image_path = images_dir.join(filename);
        if image_path.exists() {
            tasks.push(FlashTask2 {
                action: FlashAction::Flash(partition.to_string(), image_path),
            });
        }
    }

    tasks.push(FlashTask2 {
        action: FlashAction::SetActive("a".to_string()),
    });

    Ok(FlashScript { tasks })
}

fn scan_standard_images(dir: &Path) -> Result<FlashScript, FastbootError> {
    let mut tasks = Vec::new();

    let images = partition::get_standard_images();

    for img in &images {
        let image_path = dir.join(&img.img_name);
        if image_path.exists() {
            tasks.push(FlashTask2 {
                action: FlashAction::Flash(img.part_name.clone(), image_path),
            });
        } else if !img.optional {
            return Err(FastbootError::ImageNotFound(img.img_name.clone()));
        }
    }

    Ok(FlashScript { tasks })
}

async fn cmd_update(serial: &Option<String>, filename: &Path) -> Result<(), FastbootError> {
    if !filename.exists() {
        return Err(FastbootError::ImageNotFound(filename.display().to_string()));
    }

    print_info(&format!("从 {} 更新...", filename.display()));

    let source = flash::ZipImageSource::from_path(filename)
        .map_err(FastbootError::Io)?;

    let plan = flash::FlashingPlan::default();
    let tool = flash::FlashAllTool::new(source, plan);

    tool.validate()?;

    let tasks = tool.generate_tasks().map_err(FastbootError::Io)?;

    if tasks.is_empty() {
        return Err(FastbootError::InvalidArg(
            "ZIP 包中没有找到可刷写的镜像".to_string()
        ));
    }

    print_info(&format!("将刷写 {} 个分区", tasks.len()));

    let transport = open_transport(serial).await?;
    let mut driver = driver::FastbootDriver::new(transport);

    for (i, task) in tasks.iter().enumerate() {
        println!("\n[{}/{}] 刷写 {} -> {}", i + 1, tasks.len(), task.filename, task.partition);

        let data = tool.source().read_file(&task.filename)
            .map_err(FastbootError::Io)?;

        let mut pb = SimpleProgressBar::new(data.len() as u64, "Sending");

        driver.download(&data).await?;
        pb.update(data.len() as u64 / 2);

        driver.flash(&task.partition).await?;
        pb.finish();
    }

    print_success("\n更新完成!");
    Ok(())
}

async fn cmd_upload(
    serial: &Option<String>,
    partition: &str,
    filename: &Path,
) -> Result<(), FastbootError> {
    let transport = open_transport(serial).await?;
    let mut driver = driver::FastbootDriver::new(transport);

    print_info(&format!("读取分区 '{}' 到 '{}'...", partition, filename.display()));

    let size = driver.read_partition(partition, filename).await?;

    print_success(&format!("读取完成: {}", format_size(size)));
    Ok(())
}

async fn cmd_diagnose() -> Result<(), FastbootError> {
    println!("=== Fastboot 诊断 ===\n");

    println!("检查 USB 设备...");
    let devices = UsbTransport::enumerate_devices()
        .map_err(FastbootError::Transport)?;

    if devices.is_empty() {
        println!("  未发现 fastboot 设备\n");
        println!("排查建议:");
        println!("  1. 确认设备已进入 fastboot 模式");
        println!("  2. 检查 USB 线连接");
        println!("  3. 尝试其他 USB 端口（推荐 USB 3.0）");
        #[cfg(target_os = "linux")]
        println!("  4. 检查 udev 规则");
        #[cfg(target_os = "windows")]
        println!("  4. 检查 fastboot 驱动是否已安装");
    } else {
        println!("  发现 {} 个设备:\n", devices.len());
        for dev in &devices {
            println!("  设备: {}", dev.serial_number);
            println!("    VID:PID = {:04x}:{:04x}", dev.vendor_id, dev.product_id);
            println!("    USB 版本: {}", if dev.is_usb3 { "3.0 (SuperSpeed)" } else { "2.0 (High Speed)" });
            if let Some(ref name) = dev.product_name {
                println!("    产品名: {}", name);
            }
            if let Some(ref mfr) = dev.manufacturer {
                println!("    制造商: {}", mfr);
            }
            println!();
        }

        if let Some(dev) = devices.first() {
            println!("尝试连接 {}...", dev.serial_number);
            match UsbTransport::open(Some(&dev.serial_number)) {
                Ok(transport) => {
                    let mut driver = driver::FastbootDriver::new(transport);
                    match driver.get_var("version").await {
                        Ok(version) => {
                            print_success(&format!("连接成功! Fastboot 版本: {}", version));
                        }
                        Err(e) => {
                            print_error(&format!("连接失败: {}", e));
                        }
                    }
                }
                Err(e) => {
                    print_error(&format!("无法打开设备: {}", e));
                }
            }
        }
    }

    println!("\n=== 诊断完成 ===");
    Ok(())
}

async fn open_transport(serial: &Option<String>) -> Result<UsbTransport, FastbootError> {
    let devices = UsbTransport::enumerate_devices()
        .map_err(FastbootError::Transport)?;

    if devices.is_empty() {
        return Err(FastbootError::NoDevice);
    }

    if devices.len() > 1 && serial.is_none() {
        eprintln!("发现多个设备:");
        for dev in &devices {
            eprintln!("  {} - {}", dev.serial_number, dev.product_name.as_deref().unwrap_or("Unknown"));
        }
        return Err(FastbootError::MultipleDevices);
    }

    UsbTransport::open(serial.as_deref())
        .map_err(FastbootError::Transport)
}

fn connect_adb(serial: Option<&str>, verbose: bool) -> Result<adb::client::AdbClient, FastbootError> {
    use adb::client::AdbClient;

    AdbClient::connect_fast(serial, verbose)
        .map_err(|e| FastbootError::Adb(e.to_string()))
}

async fn cmd_adb_shell(serial: &Option<String>, command: &[String], verbose: bool) -> Result<(), FastbootError> {
    let mut client = connect_adb(serial.as_deref(), verbose)?;

    if command.is_empty() {
        client.interactive_shell()
            .map_err(|e| FastbootError::Adb(e.to_string()))?;
        return Ok(());
    }

    let cmd = command.join(" ");
    let output = client.shell(&cmd)
        .map_err(|e| FastbootError::Adb(e.to_string()))?;

    print!("{}", output);
    Ok(())
}

async fn cmd_adb_push(serial: &Option<String>, local: &Path, remote: &str, verbose: bool) -> Result<(), FastbootError> {
    if !local.exists() {
        return Err(FastbootError::ImageNotFound(local.display().to_string()));
    }

    let file_size = fs::metadata(local).map_err(FastbootError::Io)?.len();
    let mut client = connect_adb(serial.as_deref(), verbose)?;

    client.push(local, remote)
        .map_err(|e| FastbootError::Adb(e.to_string()))?;

    println!("{}: {} pushed", local.display(), format_size(file_size));
    Ok(())
}

async fn cmd_adb_pull(serial: &Option<String>, remote: &str, local: &Path, verbose: bool) -> Result<(), FastbootError> {
    let mut client = connect_adb(serial.as_deref(), verbose)?;

    let size = client.pull(remote, local)
        .map_err(|e| FastbootError::Adb(e.to_string()))?;

    println!("{}: {} pulled", remote, format_size(size));
    Ok(())
}

async fn cmd_adb_install(serial: &Option<String>, apk: &Path, replace: bool) -> Result<(), FastbootError> {
    if !apk.exists() {
        return Err(FastbootError::ImageNotFound(apk.display().to_string()));
    }

    let mut client = connect_adb(serial.as_deref(), false)?;

    let remote_path = format!("/data/local/tmp/{}", apk.file_name().unwrap().to_string_lossy());
    client.push(apk, &remote_path)
        .map_err(|e| FastbootError::Adb(e.to_string()))?;

    let install_cmd = if replace {
        format!("pm install -r {}", remote_path)
    } else {
        format!("pm install {}", remote_path)
    };

    let output = client.shell(&install_cmd)
        .map_err(|e| FastbootError::Adb(e.to_string()))?;

    let _ = client.shell(&format!("rm {}", remote_path));

    if output.contains("Success") {
        println!("Success");
    } else {
        print!("{}", output);
    }
    Ok(())
}

async fn cmd_adb_uninstall(serial: &Option<String>, package: &str) -> Result<(), FastbootError> {
    let mut client = connect_adb(serial.as_deref(), false)?;

    let output = client.shell(&format!("pm uninstall {}", package))
        .map_err(|e| FastbootError::Adb(e.to_string()))?;

    print!("{}", output);
    Ok(())
}

async fn cmd_adb_packages(serial: &Option<String>, third_party: bool, system: bool) -> Result<(), FastbootError> {
    let mut client = connect_adb(serial.as_deref(), false)?;

    let cmd = if third_party {
        "pm list packages -3"
    } else if system {
        "pm list packages -s"
    } else {
        "pm list packages"
    };

    let output = client.shell(cmd)
        .map_err(|e| FastbootError::Adb(e.to_string()))?;

    for line in output.lines() {
        if let Some(pkg) = line.strip_prefix("package:") {
            println!("{}", pkg);
        } else {
            println!("{}", line);
        }
    }
    Ok(())
}

async fn cmd_adb_logcat(serial: &Option<String>, filter: &[String]) -> Result<(), FastbootError> {
    let mut client = connect_adb(serial.as_deref(), false)?;

    let cmd = if filter.is_empty() {
        "logcat -d".to_string()
    } else {
        format!("logcat -d {}", filter.join(" "))
    };

    let output = client.shell(&cmd)
        .map_err(|e| FastbootError::Adb(e.to_string()))?;

    print!("{}", output);
    Ok(())
}

async fn cmd_adb_screencap(serial: &Option<String>, output: &Path) -> Result<(), FastbootError> {
    let mut client = connect_adb(serial.as_deref(), false)?;

    let remote_path = "/data/local/tmp/screenshot.png";
    client.shell(&format!("screencap -p {}", remote_path))
        .map_err(|e| FastbootError::Adb(e.to_string()))?;

    client.pull(remote_path, output)
        .map_err(|e| FastbootError::Adb(e.to_string()))?;

    let _ = client.shell(&format!("rm {}", remote_path));

    println!("Screenshot saved to {}", output.display());
    Ok(())
}

async fn cmd_adb_screenrecord(serial: &Option<String>, output: &Path, time: u32) -> Result<(), FastbootError> {
    let mut client = connect_adb(serial.as_deref(), false)?;

    let remote_path = "/data/local/tmp/recording.mp4";
    println!("Recording for {} seconds... (Ctrl+C to stop early)", time);

    let _ = client.shell(&format!("screenrecord --time-limit {} {}", time, remote_path));

    client.pull(remote_path, output)
        .map_err(|e| FastbootError::Adb(e.to_string()))?;

    let _ = client.shell(&format!("rm {}", remote_path));

    println!("Recording saved to {}", output.display());
    Ok(())
}