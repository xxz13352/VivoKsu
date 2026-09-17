//! 崩溃报告补传(P0)—— 下次启动时把上次进程 panic(`%LOCALAPPDATA%\Nwflash\crash.log`)
//! 上报到 `POST /api/diagnostics/crash`。
//!
//! 流程:启动后台延迟读取 crash.log → 逐条解析 `[epoch] panic: ...` 行 →
//! 价值过滤(复用 trace-v2 同一管线:哈希值/凭据材料这类对排障毫无意义的
//! 内容不上传——私钥块、已知凭据被整块拒绝或替换;路径、序列号等有诊断
//! 价值的内容**保留**)→ 截断到服务端上限 → 上传。
//! 上传成功(202/200)后清空 crash.log;失败保留原文件,下次启动重试。
//! 匿名可报:启动时通常尚未登录,先以匿名上传;若届时已持 token 则带 token。

use std::{
    path::PathBuf,
    sync::{Arc, RwLock},
};

use nwflash_infrastructure::{CloudflareClient, CrashReportRequest, SecretToken};
use tokio::time::{sleep, Duration};

/// 启动后延迟多久再读 crash.log:避免与登录/版本门禁的启动关键路径竞争。
const STARTUP_DELAY: Duration = Duration::from_secs(8);
/// crash.log 单文件读取上限;超过即只取末尾(最近的 panic 更有价值)。
const MAX_CRASH_LOG_BYTES: u64 = 128 * 1024;

/// 上次崩溃记录的内存形态(解析后、过滤前)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingCrashEntry {
    pub occurred_at: i64,
    pub panic_text: String,
}

/// 从 crash.log 原文解析崩溃条目(每行 `[epoch] panic: ...`)。
/// 忽略格式损坏的行;epoch 回退到 0(验证层随后拒绝,由调用方决定丢弃)。
pub(crate) fn parse_crash_log(contents: &str) -> Vec<PendingCrashEntry> {
    contents
        .lines()
        .filter_map(|line| {
            let (timestamp, text) = line.split_once("] ")?;
            let epoch = timestamp.trim_start_matches('[').parse::<i64>().ok()?;
            let text = text.strip_prefix("panic: ")?;
            Some(PendingCrashEntry {
                occurred_at: epoch,
                panic_text: text.to_string(),
            })
        })
        .collect()
}

/// 价值过滤(与 trace-v2 上传同一管线):哈希值、凭据材料这类上传后对排障
/// 没有任何意义的内容不上送——私钥块、已知凭据、超长行被管线整块拒绝或
/// 替换为 `[CREDENTIAL_REMOVED:HIGH_RISK]`;路径、序列号、命令行等有诊断
/// 价值的内容原样通过。`TraceOutputSession::from_reader` 是唯一公开的完整流
/// 扫描入口;崩溃文本里没有会话密钥,`ExactSecretSet::empty()` 即只启用
/// 模式匹配。分块结果拼回单段文本;扫描整体拒绝(私钥等)返回 None,
/// 调用方丢弃该文本不上送。
pub(crate) fn redact_crash_text(text: &str) -> Option<String> {
    use std::io::Cursor;

    use nwflash_domain::{TraceId, TraceOutputStreamV2};
    use nwflash_protection::{ExactSecretSet, TraceOutputSession};

    if text.is_empty() {
        return Some(String::new());
    }
    let event_id = TraceId::try_new_v7().ok()?;
    let secrets = ExactSecretSet::empty();
    let mut reader = Cursor::new(text.as_bytes());
    let session = TraceOutputSession::from_reader(
        event_id,
        TraceOutputStreamV2::Stdout,
        &mut reader,
        &secrets,
    )
    // 私钥等高危内容被整体拒绝:宁可丢内容也不送原文。
    .ok()?;
    let uploads = session.into_upload_attempts().ok()?;
    let mut redacted = String::with_capacity(text.len());
    for upload in &uploads {
        for chunk in upload.output_chunks() {
            redacted.push_str(chunk.text());
        }
    }
    Some(redacted)
}

/// 构造一次补传请求:panic 文本经价值过滤后按服务端上限截断。
/// 最新一条 panic 作为 `panic_message`,更早的条目拼入 `backtrace`,
/// 整体仍受 16 KiB / 32 KiB 上限约束。
pub(crate) fn build_crash_report(
    entries: &[PendingCrashEntry],
    client_version: &str,
    build_id: &str,
    session_id: &str,
) -> Option<CrashReportRequest> {
    let newest = entries.last()?;
    let panic_len = newest.panic_text.len();
    let occurred_at = newest.occurred_at;
    let event_id = format!("crash-{occurred_at}-{panic_len}");
    // event_id 只允许 URL-safe 标识字符;panic 行长度可变,用受控拼接。
    let event_id = sanitize_identifier(&event_id);

    let mut backtrace = String::new();
    for entry in entries.iter().rev().skip(1) {
        backtrace.push_str(&entry.panic_text);
        backtrace.push('\n');
    }

    let panic_message = redact_crash_text(&truncate_utf8(
        &newest.panic_text,
        CrashReportRequest::MAX_PANIC_MESSAGE_BYTES,
    ))?;
    let backtrace = redact_crash_text(&truncate_utf8(
        &backtrace,
        CrashReportRequest::MAX_BACKTRACE_BYTES,
    ))
    .unwrap_or_default();

    if panic_message.is_empty() {
        return None;
    }
    Some(CrashReportRequest {
        event_id,
        client_version: client_version.to_string(),
        build_id: build_id.to_string(),
        session_id: session_id.to_string(),
        panic_message,
        backtrace,
        occurred_at: newest.occurred_at.clamp(1, 9_007_199_254_740_991),
    })
}

fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

fn sanitize_identifier(value: &str) -> String {
    let mut out = String::with_capacity(value.len().min(64));
    for ch in value.chars() {
        if out.len() >= 64 {
            break;
        }
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | ':' | '-') {
            out.push(ch);
        } else {
            out.push('-');
        }
    }
    out
}

fn crash_log_path(base: Option<PathBuf>) -> PathBuf {
    base.unwrap_or_else(default_crash_dir).join("crash.log")
}

fn default_crash_dir() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_default();
    PathBuf::from(base).join("Nwflash")
}

/// 后台启动入口:延迟读取 → 价值过滤 → 上传 → 成功后清空。
/// 所有失败路径静默退出(保留 crash.log 供下次启动重试),绝不阻塞启动。
pub(crate) async fn run_pending_crash_upload(
    client: CloudflareClient,
    session_token: Arc<RwLock<Option<SecretToken>>>,
    client_version: String,
    build_id: String,
    session_id: String,
    crash_dir: Option<PathBuf>,
) {
    sleep(STARTUP_DELAY).await;

    let path = crash_log_path(crash_dir);
    let contents = match read_bounded(&path).await {
        Ok(contents) => contents,
        Err(_) => return,
    };
    if contents.is_empty() {
        return;
    }

    let entries = parse_crash_log(&contents);
    let report = match build_crash_report(&entries, &client_version, &build_id, &session_id) {
        Some(report) => report,
        None => return,
    };

    // 匿名优先:启动补传通常发生在登录前;若已持 token 则升级为 trusted。
    // SecretToken 不可 Clone;request_scope() 是唯一受支持的拥有型拷贝。
    let token = session_token
        .read()
        .ok()
        .and_then(|guard| guard.as_ref().map(SecretToken::request_scope));
    let outcome = match token {
        Some(token) => client.upload_crash_report(Some(&token), &report).await,
        None => client.upload_crash_report(None, &report).await,
    };

    if outcome.is_ok() {
        // 清空而非删除:crash hook 以 append 模式持有同一路径约定。
        let _ = tokio::fs::write(&path, b"").await;
    }
}

async fn read_bounded(path: &PathBuf) -> std::io::Result<String> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    let metadata = tokio::fs::metadata(path).await?;
    let take_from = metadata.len().saturating_sub(MAX_CRASH_LOG_BYTES);
    let mut file = tokio::fs::File::open(path).await?;
    if take_from > 0 {
        file.seek(std::io::SeekFrom::Start(take_from)).await?;
    }
    let mut buffer = Vec::with_capacity(MAX_CRASH_LOG_BYTES.min(metadata.len()) as usize + 1);
    file.take(MAX_CRASH_LOG_BYTES)
        .read_to_end(&mut buffer)
        .await?;
    Ok(String::from_utf8_lossy(&buffer).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(at: i64, text: &str) -> PendingCrashEntry {
        PendingCrashEntry {
            occurred_at: at,
            panic_text: text.to_string(),
        }
    }

    #[test]
    fn parse_extracts_only_panic_lines_with_epoch_prefix() {
        let log = "[1787444800] panic: panicked at src/main.rs:42:5:\nmessage\n[1787444900] not a panic\n[bad] panic: malformed timestamp\n[1787445000] panic: second panic";
        let entries = parse_crash_log(log);

        assert_eq!(
            entries,
            vec![
                entry(1787444800, "panicked at src/main.rs:42:5:"),
                entry(1787445000, "second panic"),
            ]
        );
    }

    #[test]
    fn build_report_uses_newest_panic_as_message_and_older_as_backtrace() {
        let entries = vec![entry(1000, "old panic"), entry(2000, "newest panic")];
        let report = build_crash_report(&entries, "1.4.0", "build-x", "session-x").unwrap();

        assert_eq!(report.panic_message, "newest panic");
        assert_eq!(report.backtrace, "old panic\n");
        assert_eq!(report.occurred_at, 2000);
        assert!(report.event_id.starts_with("crash-2000-"));
        assert_eq!(report.client_version, "1.4.0");
    }

    #[test]
    fn build_report_sanitizes_event_id_and_bounded_occurrence() {
        let entries = vec![entry(i64::MAX, "boom with spaces!!")];
        let report = build_crash_report(&entries, "1.4.0", "b", "s").unwrap();

        assert_eq!(report.occurred_at, 9_007_199_254_740_991);
        assert!(report
            .event_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-')));
    }

    #[test]
    fn build_report_truncates_utf8_safely() {
        let long = "panic字".repeat(9000);
        let entries = vec![entry(1, &long)];
        let report = build_crash_report(&entries, "1.4.0", "b", "s").unwrap();

        assert!(report.panic_message.len() <= CrashReportRequest::MAX_PANIC_MESSAGE_BYTES);
        assert!(report
            .panic_message
            .is_char_boundary(report.panic_message.len()));
    }

    #[test]
    fn build_report_returns_none_without_entries_or_empty_message() {
        assert!(build_crash_report(&[], "1.4.0", "b", "s").is_none());
        assert!(build_crash_report(&[entry(1, "")], "1.4.0", "b", "s").is_none());
    }

    #[test]
    fn redaction_replaces_private_key_blocks_in_panic_text() {
        // 含私钥块的文本被过滤管线整体拒绝(HighRisk fail-closed),
        // build_crash_report 丢弃该条 panic 不上送。
        let entries = vec![entry(
            42,
            "panicked with -----BEGIN RSA PRIVATE KEY-----\nsecret\n-----END KEY-----",
        )];
        assert!(build_crash_report(&entries, "1.4.0", "b", "s").is_none());
    }

    #[test]
    fn redaction_keeps_plain_panic_text() {
        let entries = vec![entry(42, "panicked at src/main.rs:42:5")];
        let report = build_crash_report(&entries, "1.4.0", "b", "s").unwrap();

        assert_eq!(report.panic_message, "panicked at src/main.rs:42:5");
    }
}
