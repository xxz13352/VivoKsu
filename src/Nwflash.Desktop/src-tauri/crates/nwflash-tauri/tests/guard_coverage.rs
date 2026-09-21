//! 守卫覆盖契约：**每个"写类"命令的入口第一行必须有守卫**。
//!
//! ## 为什么需要这条测试
//!
//! `guard_write_command` 的价值完全取决于"是否被调用"。它没有任何机制
//! 在"漏写"时失败——`cargo check` 会通过、测试会通过、`clippy` 会通过。
//! 也就是说：**漏写是静默的**。
//!
//! 唯一能把它变成显式失败的办法是**源码级契约测试**：解析命令定义，
//! 要求每个被分类为写类的命令体内出现守卫调用。
//!
//! 这不是"测试实现细节"——写类命令集合本身就是安全契约的一部分。

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// 写类命令：会写入设备分区、修改设备文件系统、或在本机产生提权副作用。
///
/// 每一条都必须在其命令体内调用 `guard_write_command`。
///
/// 加入这个列表 = 声明"此命令需要本地签名租约复检"。移除 = 声明
/// "此命令不再有写副作用"，必须在 code review 里给出理由。
const WRITE_COMMANDS: &[&str] = &[
    // 设备分区写入
    "device_reboot_system",
    "device_reboot_bootloader",
    "device_reboot_fastboot",
    "partitions_execute_write",
    "partitions_execute_erase",
    "partitions_execute_backup",
    "safe_flash_execute_prepared",
    "quick_flash_execute_boot_image",
    "quick_flash_execute_preset_image",
    "quick_flash_execute_preset_images",
    "quick_flash_execute_firmware_artifact",
    "quick_flash_execute_prepared_dual_slot_preset",
    // ROOT 流程（修补后写回设备）
    "root_install_manager",
    "root_patch_vivo_ksu",
    "root_patch_official_vendor_boot",
    "root_execute_patched_artifact_flash",
    "root_run_automatic",
    // 设备文件系统写入
    "files_delete",
    "files_upload",
    "files_install_apk",
    // 本机提权/安装副作用
    "driver_reinstall",
    "resource_install",
    // 投屏（拉起本机进程 + 设备端服务）
    "mirror_start",
];

/// 只读命令：明确**不需要**守卫。显式列出，避免将来把只读命令误判为缺口，
/// 也避免有人把写类命令悄悄挪进这个列表来"让测试变绿"。
const READ_ONLY_COMMANDS: &[&str] = &[
    "auth_login",
    "auth_logout",
    "auth_validate_token",
    "device_refresh",
    "device_identity_refresh",
    "driver_status",
    "files_list",
    "files_download",
    "firmware_inspect_local",
    "firmware_inspect_remote",
    "firmware_inspect_line_flash_package",
    "firmware_inspect_payload_local",
    "firmware_extract_payload_local",
    "firmware_extract_remote",
    "firmware_extract_vivo_local",
    "firmware_prepare_extracted_artifact",
    "firmware_prepare_line_flash_artifact",
    "firmware_select_output_directory",
    "partitions_refresh",
    "partitions_cached_snapshot",
    "partitions_map_images",
    "partitions_prepare_write",
    "partitions_prepare_erase",
    "partitions_prepare_backup",
    "quick_flash_inspect_image",
    "quick_flash_prepare_boot_image",
    "quick_flash_prepare_preset_image",
    "quick_flash_prepare_firmware_artifact",
    "quick_flash_prepare_dual_slot_preset_image",
    "mirror_status",
    "mirror_stop",
    "mirror_set_auto",
    "online_sessions",
    "operation_cancel",
    "operation_logs_clear",
    "operation_logs_snapshot",
    "resource_inventory",
    "root_preflight",
    "root_select_image",
    "root_export_patched_artifact",
    "root_prepare_patched_artifact_flash",
    "root_ota_extract_images",
    "safe_flash_prepare_local_source",
    "safe_flash_prepare_local_directory",
    "safe_flash_prepare_online",
    "safe_flash_cancel_prepared",
    "safe_flash_resolve_partition_failure",
    "authorize",
    "some_write_command",
    "root_ota_check",
    "session_start",
    "session_state",
    "session_stop",
    "software_status",
    "version_check",
];

fn commands_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join("commands")
}

/// 找到 `<name>` 命令体的源码文本（从 `#[tauri::command]` 到下一个
/// `#[tauri::command]` 或文件末尾）。
fn command_bodies() -> Vec<(String, String)> {
    let mut bodies = Vec::new();
    let mut files: Vec<_> = fs::read_dir(commands_dir())
        .expect("commands directory must be readable")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "rs"))
        .collect();
    files.sort();

    for file in files {
        let text = fs::read_to_string(&file).expect("command source must be readable");
        let mut cursor = 0usize;
        while let Some(found) = text[cursor..].find("#[tauri::command]") {
            let start = cursor + found;
            let rest = &text[start + "#[tauri::command]".len()..];
            let end = rest
                .find("#[tauri::command]")
                .map(|offset| start + "#[tauri::command]".len() + offset)
                .unwrap_or(text.len());
            let segment = &text[start..end];
            if let Some(name) = extract_fn_name(segment) {
                bodies.push((name, segment.to_string()));
            }
            cursor = end;
            if cursor >= text.len() {
                break;
            }
        }
    }
    bodies
}

fn extract_fn_name(segment: &str) -> Option<String> {
    let marker = "fn ";
    let mut search = 0usize;
    while let Some(found) = segment[search..].find(marker) {
        let absolute = search + found + marker.len();
        let tail = &segment[absolute..];
        let name: String = tail
            .chars()
            .take_while(|ch| ch.is_alphanumeric() || *ch == '_')
            .collect();
        // 跳过 `#[tauri::command]` 之后可能出现的宏/注释里的 "fn "。
        if !name.is_empty() {
            return Some(name);
        }
        search = absolute;
    }
    None
}

#[test]
fn every_write_command_calls_the_entry_guard() {
    let bodies = command_bodies();
    assert!(
        !bodies.is_empty(),
        "no tauri commands were discovered; the source parser is broken"
    );

    let mut missing = Vec::new();
    for (name, body) in &bodies {
        if WRITE_COMMANDS.contains(&name.as_str()) && !body.contains("guard_write_command") {
            missing.push(name.clone());
        }
    }

    assert!(
        missing.is_empty(),
        "写类命令缺少入口守卫 (guard_write_command): {missing:?}\n\
         这些命令会写入设备或产生提权副作用，但没有任何本地准入校验。"
    );
}

#[test]
fn the_write_and_read_only_lists_cover_every_declared_command() {
    let names: BTreeSet<String> = command_bodies().into_iter().map(|(name, _)| name).collect();
    let classified: BTreeSet<&str> = WRITE_COMMANDS
        .iter()
        .chain(READ_ONLY_COMMANDS.iter())
        .copied()
        .collect();

    let unclassified: Vec<&String> = names
        .iter()
        .filter(|name| !classified.contains(name.as_str()))
        .collect();
    assert!(
        unclassified.is_empty(),
        "以下命令既未列为写类也未列为只读，必须显式分类: {unclassified:?}"
    );
}

#[test]
fn no_command_is_classified_twice() {
    let write: BTreeSet<&str> = WRITE_COMMANDS.iter().copied().collect();
    let read_only: BTreeSet<&str> = READ_ONLY_COMMANDS.iter().copied().collect();
    let overlap: Vec<&&str> = write.intersection(&read_only).collect();
    assert!(
        overlap.is_empty(),
        "同一命令不能同时是写类和只读: {overlap:?}"
    );
}

