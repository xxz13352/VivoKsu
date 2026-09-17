//! 命令级使用日志明细：把每一次进程执行写成一条上报服务器的明细。
//!
//! 背景：使用日志的 `details` 过去只有阶段级文案（「正在刷写」「执行 1/5」），
//! 排障时看不出到底跑了哪条命令、退出码多少、fastboot 回了什么。本模块把
//! [`ProcessCommandRecorder`] 挂到真实进程执行器上，逐条命令产出
//! `[cmd] <argv> → exit=<code> · <耗时> · out: … · err: …` 明细。
//!
//! 两条硬约束：
//!
//! 1. **只进上报服务器的明细，不写本地操作日志区**——工具界面的日志保持原样
//!    （见 [`OperationContext::report_usage_detail`]）。
//! 2. **回调是同步且位于进程观测线程上**：这里只做格式化 + 入桶，不做磁盘 I/O、
//!    HTTP 或阻塞等待。
//!
//! 连续重复的同一条命令（例如等待 fastbootd 的 `fastboot devices` 轮询）会被
//! 折叠成一条，避免 360 次轮询把 500 条的明细分桶刷满、挤掉真正有用的刷写记录。

use std::{
    path::Path,
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};

use nwflash_windows::process::{ProcessCommandRecord, ProcessCommandRecorder, ProcessTermination};

use crate::{sanitize_operation_detail, OperationContext};

/// 单条命令留痕里 stdout / stderr 各自的字符上限。命令输出可以是上百 MiB 的
/// 刷写日志，留痕只保留开头一段用于判定协议错误。
pub const DEFAULT_OUTPUT_CHARS: usize = 300;

/// 连续重复计数的上限（纯防御：计数只用于文案，不需要精确到无穷）。
const MAX_REPEAT_COUNT: usize = 100_000;

/// 明细的接收端。抽出 trait 只为一件事：让本模块的格式化/折叠逻辑可以在
/// 没有协调器的前提下被单元测试覆盖。
pub trait UsageDetailSink: Send + Sync {
    fn report(&self, detail: String);
}

impl UsageDetailSink for OperationContext {
    fn report(&self, detail: String) {
        self.report_usage_detail(detail);
    }
}

/// 把每条已执行命令写成命令级明细。
///
/// 由 [`nwflash_windows::process::RecordingProcessExecutor`] 驱动，一个实例可以
/// 覆盖整个操作（多条命令），内部状态只用于折叠连续重复项。
pub struct OperationCommandRecorder<S: UsageDetailSink = OperationContext> {
    sink: S,
    output_chars: usize,
    state: StdMutex<RecorderState>,
}

#[derive(Default)]
struct RecorderState {
    last_command: Option<String>,
    repeat_count: usize,
}

impl OperationCommandRecorder<OperationContext> {
    pub fn new(context: OperationContext) -> Arc<Self> {
        Arc::new(Self::with_sink(context))
    }
}

impl<S: UsageDetailSink> OperationCommandRecorder<S> {
    pub fn with_sink(sink: S) -> Self {
        Self {
            sink,
            output_chars: DEFAULT_OUTPUT_CHARS,
            state: StdMutex::new(RecorderState::default()),
        }
    }

    pub fn with_output_chars(mut self, output_chars: usize) -> Self {
        self.output_chars = output_chars;
        self
    }
}

impl<S: UsageDetailSink> ProcessCommandRecorder for OperationCommandRecorder<S> {
    fn record(&self, record: ProcessCommandRecord<'_>) {
        let command = render_command(record.program, record.args);
        let outcome = render_outcome(record.exit_code, record.termination, record.duration);
        let mut lines: Vec<String> = Vec::new();
        {
            let mut state = self.state.lock().unwrap_or_else(|value| value.into_inner());
            if state.last_command.as_deref() == Some(command.as_str()) {
                state.repeat_count = (state.repeat_count + 1).min(MAX_REPEAT_COUNT);
                return;
            }
            if let (Some(previous), true) = (state.last_command.as_deref(), state.repeat_count > 1)
            {
                lines.push(format!(
                    "[cmd] ↻ {previous} 连续执行 {} 次（其中 {} 次重复已折叠）",
                    state.repeat_count,
                    state.repeat_count - 1
                ));
            }
            state.last_command = Some(command.clone());
            state.repeat_count = 1;
            lines.push(render_entry(&command, &outcome, &record, self.output_chars));
        }
        // 在锁外上报：`report` 会去抢明细分桶的锁，不能与自身状态锁嵌套。
        for line in lines {
            self.sink.report(line);
        }
    }
}

fn render_entry(
    command: &str,
    outcome: &str,
    record: &ProcessCommandRecord<'_>,
    output_chars: usize,
) -> String {
    let mut line = format!("[cmd] {command} → {outcome}");
    let stdout = render_stream(record.stdout, output_chars);
    if !stdout.is_empty() {
        line.push_str(" · out: ");
        line.push_str(&stdout);
    }
    let stderr = render_stream(record.stderr, output_chars);
    if !stderr.is_empty() {
        line.push_str(" · err: ");
        line.push_str(&stderr);
    }
    line
}

/// 输出先按日志区的既有脱敏规则清洗（隐藏本地路径 / URL / `token=` 一类赋值），
/// 再把换行折叠成单行——一条明细占一行才能在管理端逐行读。
fn render_stream(bytes: &[u8], output_chars: usize) -> String {
    if bytes.is_empty() || output_chars == 0 {
        return String::new();
    }
    let text = String::from_utf8_lossy(bytes);
    let sanitized = sanitize_operation_detail(&text);
    clip(&sanitized, output_chars)
}

/// `program` 通常是捆绑工具的全路径（`…\platform-tools\adb.exe`）。日志里只需要
/// 工具名：全路径既会被脱敏规则抹成 `[已隐藏]`，也没有诊断价值。
fn render_command(program: &str, args: &[String]) -> String {
    let tool = Path::new(program)
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .filter(|stem| !stem.trim().is_empty())
        .unwrap_or_else(|| program.to_string());
    let joined = std::iter::once(tool)
        .chain(args.iter().cloned())
        .collect::<Vec<_>>()
        .join(" ");
    sanitize_operation_detail(&joined)
}

fn render_outcome(
    exit_code: Option<i32>,
    termination: ProcessTermination,
    duration: Duration,
) -> String {
    let status = match termination {
        ProcessTermination::Completed => match exit_code {
            Some(code) => format!("exit={code}"),
            None => "exit=未知".to_string(),
        },
        ProcessTermination::SpawnFailed => "启动失败".to_string(),
        ProcessTermination::WaitFailed => "等待失败".to_string(),
        ProcessTermination::OutputReadFailed => "输出读取失败".to_string(),
        ProcessTermination::Cancelled => "已取消".to_string(),
        ProcessTermination::TimedOut => "超时".to_string(),
        ProcessTermination::TerminationUnconfirmed => "终止未确认".to_string(),
    };
    format!("{status} · {}", render_duration(duration))
}

fn render_duration(duration: Duration) -> String {
    let millis = duration.as_millis();
    if millis < 1_000 {
        return format!("{millis}ms");
    }
    if duration.as_secs() < 60 {
        return format!("{:.1}s", duration.as_secs_f64());
    }
    format!("{}m{}s", duration.as_secs() / 60, duration.as_secs() % 60)
}

fn clip(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let mut clipped: String = text.chars().take(limit).collect();
    clipped.push('…');
    clipped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct RecordingSink {
        lines: StdMutex<Vec<String>>,
    }

    impl RecordingSink {
        fn lines(&self) -> Vec<String> {
            self.lines
                .lock()
                .unwrap_or_else(|value| value.into_inner())
                .clone()
        }
    }

    impl UsageDetailSink for Arc<RecordingSink> {
        fn report(&self, detail: String) {
            self.lines
                .lock()
                .unwrap_or_else(|value| value.into_inner())
                .push(detail);
        }
    }

    fn recorder(sink: Arc<RecordingSink>) -> OperationCommandRecorder<Arc<RecordingSink>> {
        OperationCommandRecorder::with_sink(sink)
    }

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    fn record<'a>(
        program: &'a str,
        args: &'a [String],
        exit_code: Option<i32>,
        stdout: &'a [u8],
        stderr: &'a [u8],
    ) -> ProcessCommandRecord<'a> {
        ProcessCommandRecord {
            program,
            args,
            exit_code,
            termination: ProcessTermination::Completed,
            duration: Duration::from_millis(1_234),
            stdout,
            stderr,
            stdout_truncated: false,
            stderr_truncated: false,
        }
    }

    #[test]
    fn records_the_full_command_line_with_exit_code_and_output() {
        let sink = Arc::new(RecordingSink::default());
        let command = args(&["-s", "ABC123", "flash", "boot", "boot.img"]);
        recorder(sink.clone()).record(record(
            r"C:\tools\platform-tools\fastboot.exe",
            &command,
            Some(0),
            b"Sending 'boot' (65536 KB) OKAY [  1.5s]\nFinished. Total time: 2.1s",
            b"",
        ));

        let lines = sink.lines();
        assert_eq!(lines.len(), 1);
        // 工具名取 stem，全路径不出现在明细里。
        assert!(lines[0].starts_with("[cmd] fastboot -s ABC123 flash boot boot.img → "));
        assert!(lines[0].contains("exit=0"));
        assert!(lines[0].contains("1.2s"));
        assert!(lines[0].contains("out: Sending 'boot' (65536 KB) OKAY [ 1.5s] Finished."));
        assert!(!lines[0].contains("C:"));
        assert!(!lines[0].contains('\n'));
    }

    #[test]
    fn keeps_stderr_visible_for_failed_commands() {
        let sink = Arc::new(RecordingSink::default());
        let command = args(&["-s", "ABC123", "flash", "super", "super.img"]);
        recorder(sink.clone()).record(record(
            "fastboot",
            &command,
            Some(1),
            b"",
            b"FAILED (remote: 'Not enough space to resize partition')",
        ));

        let lines = sink.lines();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("exit=1"));
        assert!(lines[0].contains("err: FAILED (remote: 'Not enough space to resize partition')"));
    }

    #[test]
    fn hides_local_paths_and_credential_assignments() {
        let sink = Arc::new(RecordingSink::default());
        let command = args(&[
            "-s",
            "ABC123",
            "flash",
            r"C:\Users\me\staging\boot.img",
            "token=secret-value",
        ]);
        recorder(sink.clone()).record(record(
            "fastboot",
            &command,
            Some(0),
            b"",
            b"https://example.test/hook?token=secret-value",
        ));

        let lines = sink.lines();
        assert!(!lines[0].contains("C:\\Users"));
        assert!(!lines[0].contains("secret-value"));
        assert!(lines[0].contains("[已隐藏]"));
    }

    #[test]
    fn collapses_consecutive_identical_polling_commands() {
        let sink = Arc::new(RecordingSink::default());
        let recorder = recorder(sink.clone());
        let poll = args(&["devices"]);
        let flash = args(&["flash", "boot", "boot.img"]);

        for _ in 0..360 {
            recorder.record(record("fastboot", &poll, Some(0), b"", b""));
        }
        recorder.record(record("fastboot", &flash, Some(0), b"", b""));

        let lines = sink.lines();
        // 一次轮询留痕 + 一条折叠汇总 + 一次刷写 = 3 条，而不是 361 条。
        assert_eq!(lines.len(), 3, "{lines:#?}");
        assert!(lines[0].starts_with("[cmd] fastboot devices → "));
        assert_eq!(
            lines[1],
            "[cmd] ↻ fastboot devices 连续执行 360 次（其中 359 次重复已折叠）"
        );
        assert!(lines[2].starts_with("[cmd] fastboot flash boot boot.img → "));
    }

    #[test]
    fn does_not_collapse_non_consecutive_repeats() {
        let sink = Arc::new(RecordingSink::default());
        let recorder = recorder(sink.clone());
        let devices = args(&["devices"]);
        let getvar = args(&["getvar", "current-slot"]);

        recorder.record(record("fastboot", &devices, Some(0), b"", b""));
        recorder.record(record("fastboot", &getvar, Some(0), b"", b""));
        recorder.record(record("fastboot", &devices, Some(0), b"", b""));

        assert_eq!(sink.lines().len(), 3);
    }

    #[test]
    fn truncates_long_output_but_marks_it() {
        let sink = Arc::new(RecordingSink::default());
        let command = args(&["devices"]);
        let long = vec![b'x'; 2_000];
        OperationCommandRecorder::with_sink(sink.clone())
            .with_output_chars(32)
            .record(record("fastboot", &command, Some(0), &long, b""));

        let lines = sink.lines();
        assert!(lines[0].contains(&format!("out: {}…", "x".repeat(32))));
    }

    #[test]
    fn reports_non_completed_terminations_with_a_readable_label() {
        let sink = Arc::new(RecordingSink::default());
        let command = args(&["devices"]);
        let mut entry = record("fastboot", &command, None, b"", b"");
        entry.termination = ProcessTermination::TimedOut;
        entry.duration = Duration::from_secs(95);
        recorder(sink.clone()).record(entry);

        assert!(sink.lines()[0].contains("超时 · 1m35s"));
    }

    #[test]
    fn renders_sub_second_durations_in_milliseconds() {
        assert_eq!(render_duration(Duration::from_millis(42)), "42ms");
        assert_eq!(render_duration(Duration::from_millis(1_500)), "1.5s");
        assert_eq!(render_duration(Duration::from_secs(180)), "3m0s");
    }
}
