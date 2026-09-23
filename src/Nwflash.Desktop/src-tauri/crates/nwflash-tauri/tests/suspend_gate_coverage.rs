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

/// 判定一行是否为**刷写执行入口**：`SafeFlashExecutionService` 上的 `.execute*`。
///
/// 两条真实写法都要认：
///   - `execution_service.execute_with_partition_progress(`（接收端与方法名同行）
///   - `.execute_with_suspend_gate(`（链式表达式，方法名单独成行）
///
/// **不写死具体方法名**——这正是原先的盲区：旧实现只精确匹配 `.execute(` 与
/// `.execute_with_suspend_gate(` 两个字符串，于是 `safe_flash.rs` 里真实存在的
/// `.execute_with_partition_progress(` 对它**完全不可见**（既不报"绕过闸门"，
/// 也不报"未覆盖"）。将来再新增一个 `.execute_*` 入口且忘了接挂起闸门，
/// 扫描器同样看不见，缺口静默通过。
///
/// 反过来，文件事务那一套（`self.execute_with_progress(..)`、
/// `transaction.execute_with_progress(..)`）虽然同名，却属于**另一套执行器
/// 抽象**、与反调试闸门无关，必须排除——否则会误报一片。
fn is_flash_execution_call(trimmed: &str) -> bool {
    // 先摘出 `.execute*` 的方法名（含紧跟在后面的 `(`）。
    let Some(dot) = trimmed.find(".execute") else {
        return false;
    };
    let rest = &trimmed[dot + 1..];
    let name_end = rest.find('(').unwrap_or(rest.len());
    let name = &rest[..name_end];
    if name != "execute" && !name.starts_with("execute_") {
        return false;
    }
    // 接收端：同行前缀；没有前缀时（链式写法）视为合格——那种写法只出现在
    // `SafeFlashExecutionService` 的链上（`.with_executor(..).execute_*`）。
    let receiver = trimmed[..dot].trim();
    receiver.is_empty() || receiver == "execution_service"
}

/// 收集所有**真正下发刷写命令**的执行点：`SafeFlashExecutionService` 上以
/// `execute` 开头的方法调用，及其所属文件与行号。
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
        // 只扫**非测试**部分：测试模块里会有大量与本契约无关的同名调用。
        //
        // 边界取 `mod tests {` 而不是第一个 `#[cfg(test)]`：后者常出现在正文里
        // 零散的测试专用小函数上（如 `#[cfg(test)] fn replace(..)`），可能比真实
        // 刷写调用点还靠前，用它截断会把整个正文跳过、扫描器空转。
        let production_end = text.find("mod tests {").unwrap_or(text.len());
        for (index, line) in text[..production_end].lines().enumerate() {
            let trimmed = line.trim();
            if is_flash_execution_call(trimmed) {
                sites.push((name.clone(), trimmed.to_string(), index + 1));
            }
        }
    }
    sites
}

#[test]
fn every_safe_flash_execution_call_site_uses_the_suspend_gate() {
    let sites = execution_call_sites();
    // 被认可的入口是**带挂起闸门参数**的那些。按 `SafeFlashExecutionService`
    // 的实际签名，接 `is_suspended` 的正是：
    //   - `execute_with_suspend_gate(...)`
    //   - `execute_with_partition_progress(...)`（内部同样透传 `is_suspended`）
    // 而 `execute(...)` / `execute_with_partition_failure_hook(...)` 没有这个参数，
    // 走它们就绕过了闸门。这里只认名字，是因为签名无法从调用行直接读出来；
    // 名字与"是否接闸门"的对应关系由 `safe_flash.rs` 的签名保证（见其文档）。
    const GATED_ENTRIES: [&str; 2] = [
        ".execute_with_suspend_gate(",
        ".execute_with_partition_progress(",
    ];
    let ungated: Vec<_> = sites
        .iter()
        .filter(|(_, call, _)| !GATED_ENTRIES.iter().any(|entry| call.contains(entry)))
        .collect();

    assert!(
        ungated.is_empty(),
        "以下刷写执行点绕过了反调试挂起闸门（调用了 `.execute(` 而不是 \
         `.execute_with_suspend_gate(`）：\n{ungated:#?}\n\
         这会制造\"某条刷写路径会挂起、另一条不会\"的静默缺口。"
    );
}

/// 扫描器自检：真实源码里存在**多个** `execute*` 执行入口，扫描器必须都能看见。
///
/// 没有这条，"扫描器看不见任何东西"与"没有任何绕过"在测试结果上完全一样——
/// 旧的精确字符串匹配就栽在这里（只看 1 个，漏掉了 partition-progress 那个）。
#[test]
fn the_scanner_sees_every_execute_entry_point() {
    let sites = execution_call_sites();
    assert!(
        sites.len() >= 2,
        "扫描器只找到 {} 个 execute* 入口：{sites:?}",
        sites.len()
    );
    assert!(
        sites
            .iter()
            .any(|(_, call, _)| call.contains(".execute_with_partition_progress(")),
        "扫描器必须看得见 `.execute_with_partition_progress(`（旧实现的盲区）。"
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
