//! 刷写路径必须走"反调试挂起闸门"的源码级契约。
//!
//! ## 为什么需要这条测试
//!
//! `SafeFlashExecutionService` 有两个入口：
//! - `execute(...)` —— **没有**反调试挂起检查；
//! - `execute_with_suspend_gate(...)` —— 每个命令边界检查是否应挂起。
//!
//! 只有后者能保证"写入中途检测到调试器 → 暂停推进而不是继续写盘"。
//! 2026-09-21 的复审发现 `root_run_automatic` 的刷写阶段直接调用了
//! `execute(...)`，因此**同一条设备写入路径上，走 VIVO 线刷会挂起、
//! 走 ROOT 全自动却不会**——这是一个静默缺口：编译通过、测试通过。
//!
//! 这条测试解析刷写执行的源码，要求任何构造 `SafeFlashExecutionRequest`
//! 后真正下发命令的地方都必须经 `execute_with_suspend_gate`。

use std::fs;
use std::path::{Path, PathBuf};

fn commands_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join("commands")
}

/// 收集所有**真正下发刷写命令**的执行点：调用 `.execute(` 或
/// `.execute_with_suspend_gate(` 的位置及其所属文件。
fn execution_call_sites() -> Vec<(String, String, usize)> {
    let mut sites = Vec::new();
    let mut files: Vec<_> = fs::read_dir(commands_dir())
        .expect("commands directory must be readable")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "rs"))
        .collect();
    files.sort();

    for file in files {
        let name = file
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_string();
        let text = fs::read_to_string(&file).expect("command source must be readable");
        for (index, line) in text.lines().enumerate() {
            let trimmed = line.trim();
            // 只看 SafeFlashExecutionService 的执行入口，忽略 file manager
            // 等其它 `.execute(`（它们走的是不同的执行器抽象）。
            if trimmed == ".execute(" || trimmed == ".execute_with_suspend_gate(" {
                sites.push((name.clone(), trimmed.to_string(), index + 1));
            }
        }
    }
    sites
}

#[test]
fn every_safe_flash_execution_call_site_uses_the_suspend_gate() {
    let sites = execution_call_sites();
    let ungated: Vec<_> = sites
        .iter()
        .filter(|(_, call, _)| call == ".execute(")
        .collect();

    assert!(
        ungated.is_empty(),
        "以下刷写执行点绕过了反调试挂起闸门（调用了 `.execute(` 而不是 \
         `.execute_with_suspend_gate(`）：\n{ungated:#?}\n\
         这会制造\"某条刷写路径会挂起、另一条不会\"的静默缺口。"
    );
}

#[test]
fn the_suspend_gate_helper_is_the_single_construction_point() {
    // 反调试挂起查询只能由 `during_write_suspend_query` 构造。内联写
    // `AntiDebugDecision::SuspendAndWarn` 判定意味着某条路径自己造了一套
    // 判定，将来 `anti_debug` 的状态机改了它不会跟着改。
    let safe_flash = commands_dir().join("safe_flash.rs");
    let text = fs::read_to_string(&safe_flash).expect("safe_flash.rs must be readable");

    let inline_matches = text.matches("AntiDebugDecision::SuspendAndWarn").count();
    assert_eq!(
        inline_matches, 1,
        "反调试挂起判定应只在 `during_write_suspend_query` 里出现一次，\
         实际出现 {inline_matches} 次——说明有调用点内联了自己的一套判定。"
    );
    assert!(
        text.contains("pub(crate) fn during_write_suspend_query("),
        "`during_write_suspend_query` 必须存在且为 crate 可见，供所有刷写路径复用。"
    );
}
