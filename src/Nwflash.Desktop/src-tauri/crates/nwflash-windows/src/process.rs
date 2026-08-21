use std::{
    fs::File,
    io::{self, Read},
    path::{Path, PathBuf},
    process::{self, Stdio},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use nwflash_domain::DomainError;

const KILL_POLL_STEP: Duration = Duration::from_millis(10);

/// Drains a child pipe on its own thread.
///
/// The supervision loop below only polls `try_wait()`, so nothing else reads
/// these pipes while the child runs.  A child that writes more than the OS pipe
/// buffer (4 KiB - 64 KiB on Windows) would block forever on `write`, `try_wait`
/// would never report an exit, and the single-permit `OperationCoordinator`
/// would stay held — locking every page in the app until the process was killed.
type PipeReader = Option<JoinHandle<io::Result<Vec<u8>>>>;

fn spawn_pipe_reader<R>(stream: Option<R>) -> PipeReader
where
    R: Read + Send + 'static,
{
    stream.map(|mut stream| {
        thread::spawn(move || {
            let mut buffer = Vec::new();
            stream.read_to_end(&mut buffer).map(|_| buffer)
        })
    })
}

fn collect_pipe(reader: PipeReader, stream_label: &str) -> Result<Vec<u8>, DomainError> {
    let Some(reader) = reader else {
        return Ok(Vec::new());
    };

    match reader.join() {
        Ok(Ok(bytes)) => Ok(bytes),
        Ok(Err(_)) | Err(_) => Err(DomainError::ExternalTool(format!(
            "读取命令 {stream_label} 输出失败。"
        ))),
    }
}

fn reap_pipe(reader: PipeReader, stream_label: &str) {
    let _ = collect_pipe(reader, stream_label);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessCommand {
    pub program: String,
    pub args: Vec<String>,
    pub working_directory: Option<PathBuf>,
    pub environment: Vec<(String, String)>,
}

impl ProcessCommand {
    pub fn new(program: impl Into<String>, args: impl IntoIterator<Item = String>) -> Self {
        Self {
            program: program.into(),
            args: args.into_iter().collect(),
            working_directory: None,
            environment: Vec::new(),
        }
    }

    pub fn with_env(mut self, environment: Vec<(String, String)>) -> Self {
        self.environment = environment;
        self
    }

    pub fn with_working_directory(mut self, working_directory: PathBuf) -> Self {
        self.working_directory = Some(working_directory);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessOutput {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

pub trait CancellableProcessExecutor: Send + Sync {
    fn run(
        &self,
        spec: ProcessCommand,
        should_cancel: &mut dyn FnMut() -> bool,
    ) -> Result<ProcessOutput, DomainError>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemCancellableProcessExecutor;

impl CancellableProcessExecutor for SystemCancellableProcessExecutor {
    fn run(
        &self,
        spec: ProcessCommand,
        should_cancel: &mut dyn FnMut() -> bool,
    ) -> Result<ProcessOutput, DomainError> {
        run_command_with_cancel(spec, None, should_cancel)
    }
}

pub fn validate_command(program: &str) -> Result<&str, DomainError> {
    let normalized = program.trim();
    if normalized.is_empty() {
        return Err(DomainError::InvalidInput("命令不能为空。".to_string()));
    }

    crate::platform_tools::verify_if_bundled_platform_tool(Path::new(normalized))?;

    Ok(normalized)
}

pub fn validate_args(args: &[String]) -> Result<(), DomainError> {
    for arg in args {
        if arg.trim().is_empty() {
            return Err(DomainError::InvalidInput("参数不能为空。".to_string()));
        }
    }

    Ok(())
}

pub fn validate_working_directory(path: &Path) -> Result<(), DomainError> {
    if !path.exists() {
        return Err(DomainError::InvalidInput(format!(
            "工作目录不存在: {}",
            path.to_string_lossy()
        )));
    }

    Ok(())
}

pub fn validate_environment(environment: &[(String, String)]) -> Result<(), DomainError> {
    for (key, value) in environment {
        if key.trim().is_empty() {
            return Err(DomainError::InvalidInput(
                "环境变量名不能为空。".to_string(),
            ));
        }

        if value.is_empty() {
            return Err(DomainError::InvalidInput(format!(
                "环境变量 {key} 的值不能为空。"
            )));
        }
    }

    Ok(())
}

pub fn run_command(spec: ProcessCommand) -> Result<ProcessOutput, DomainError> {
    run_command_with_cancel(spec, None, || false)
}

pub fn run_command_with_timeout(
    spec: ProcessCommand,
    timeout: Option<Duration>,
) -> Result<ProcessOutput, DomainError> {
    run_command_with_cancel(spec, timeout, || false)
}

pub fn run_command_with_cancel<F>(
    spec: ProcessCommand,
    timeout: Option<Duration>,
    should_cancel: F,
) -> Result<ProcessOutput, DomainError>
where
    F: FnMut() -> bool,
{
    run_command_with_optional_file_stdin(spec, None, timeout, should_cancel)
}

pub fn run_command_with_file_stdin_and_cancel<F>(
    spec: ProcessCommand,
    input_path: &std::path::Path,
    timeout: Option<Duration>,
    should_cancel: F,
) -> Result<ProcessOutput, DomainError>
where
    F: FnMut() -> bool,
{
    let input = File::open(input_path)
        .map_err(|error| DomainError::InvalidInput(format!("无法打开命令输入文件：{error}")))?;
    run_command_with_optional_file_stdin(spec, Some(input), timeout, should_cancel)
}

pub fn run_command_with_file_stdout_and_cancel<F>(
    spec: ProcessCommand,
    output_path: &std::path::Path,
    timeout: Option<Duration>,
    mut should_cancel: F,
) -> Result<ProcessOutput, DomainError>
where
    F: FnMut() -> bool,
{
    let program = validate_command(&spec.program)?;
    validate_args(&spec.args)?;
    validate_environment(&spec.environment)?;
    if let Some(work_dir) = &spec.working_directory {
        validate_working_directory(work_dir)?;
    }
    let output_file = File::create(output_path)
        .map_err(|error| DomainError::InvalidInput(format!("无法创建命令输出文件：{error}")))?;

    let mut command = process::Command::new(program);
    command.args(&spec.args);
    if let Some(work_dir) = &spec.working_directory {
        command.current_dir(work_dir);
    }
    command.envs(spec.environment);
    command.stdout(Stdio::from(output_file));
    command.stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| DomainError::ExternalTool(format!("执行命令失败: {error}")))?;

    // stdout already goes straight to `output_path`; stderr is still a pipe and
    // must be drained concurrently or a chatty child wedges the loop below.
    let stderr_reader = spawn_pipe_reader(child.stderr.take());

    let start = Instant::now();
    loop {
        let exited = match child.try_wait() {
            Ok(status) => status.is_some(),
            Err(error) => {
                terminate_process_tree(&mut child);
                let _ = child.wait();
                reap_pipe(stderr_reader, "stderr");
                return Err(DomainError::ExternalTool(format!(
                    "等待命令结束失败: {error}"
                )));
            }
        };
        if exited {
            break;
        }
        if should_cancel() {
            terminate_process_tree(&mut child);
            let _ = child.wait();
            reap_pipe(stderr_reader, "stderr");
            return Err(DomainError::UserCancelled("运行被用户取消".to_string()));
        }
        if timeout.is_some_and(|limit| start.elapsed() >= limit) {
            terminate_process_tree(&mut child);
            let _ = child.wait();
            reap_pipe(stderr_reader, "stderr");
            return Err(DomainError::ExternalTool(
                "命令执行超时，进程已终止。".to_string(),
            ));
        }
        thread::sleep(KILL_POLL_STEP);
    }

    let status = child
        .wait()
        .map_err(|error| DomainError::ExternalTool(format!("执行命令失败: {error}")))?;
    let stderr = collect_pipe(stderr_reader, "stderr")?;
    Ok(ProcessOutput {
        exit_code: status.code().unwrap_or(-1),
        stdout: String::new(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

fn run_command_with_optional_file_stdin<F>(
    spec: ProcessCommand,
    input: Option<File>,
    timeout: Option<Duration>,
    mut should_cancel: F,
) -> Result<ProcessOutput, DomainError>
where
    F: FnMut() -> bool,
{
    let program = validate_command(&spec.program)?;
    validate_args(&spec.args)?;
    validate_environment(&spec.environment)?;

    if let Some(work_dir) = &spec.working_directory {
        validate_working_directory(work_dir)?;
    }

    let mut command = process::Command::new(program);
    for arg in &spec.args {
        command.arg(arg);
    }
    if let Some(work_dir) = &spec.working_directory {
        command.current_dir(work_dir);
    }
    command.envs(spec.environment);
    if let Some(input) = input {
        command.stdin(Stdio::from(input));
    }
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());

    let mut child = command
        .spawn()
        .map_err(|error| DomainError::ExternalTool(format!("执行命令失败: {error}")))?;

    // Both pipes must be drained while the child runs, not after it exits.
    let stdout_reader = spawn_pipe_reader(child.stdout.take());
    let stderr_reader = spawn_pipe_reader(child.stderr.take());

    let start = Instant::now();
    loop {
        let exited = match child.try_wait() {
            Ok(status) => status.is_some(),
            Err(error) => {
                terminate_process_tree(&mut child);
                let _ = child.wait();
                reap_pipe(stdout_reader, "stdout");
                reap_pipe(stderr_reader, "stderr");
                return Err(DomainError::ExternalTool(format!(
                    "等待命令结束失败: {error}"
                )));
            }
        };
        if exited {
            break;
        }

        if should_cancel() {
            terminate_process_tree(&mut child);
            let _ = child.wait();
            reap_pipe(stdout_reader, "stdout");
            reap_pipe(stderr_reader, "stderr");
            return Err(DomainError::UserCancelled("运行被用户取消".to_string()));
        }

        if let Some(timeout) = timeout {
            if start.elapsed() >= timeout {
                terminate_process_tree(&mut child);
                let _ = child.wait();
                reap_pipe(stdout_reader, "stdout");
                reap_pipe(stderr_reader, "stderr");
                return Err(DomainError::ExternalTool(
                    "命令执行超时，进程已终止。".to_string(),
                ));
            }
        }

        thread::sleep(KILL_POLL_STEP);
    }

    let status = child
        .wait()
        .map_err(|error| DomainError::ExternalTool(format!("执行命令失败: {error}")))?;

    let stdout_result = collect_pipe(stdout_reader, "stdout");
    let stderr_result = collect_pipe(stderr_reader, "stderr");
    let stdout = stdout_result?;
    let stderr = stderr_result?;
    let exit_code = status.code().unwrap_or(-1);
    Ok(ProcessOutput {
        exit_code,
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

fn terminate_process_tree(child: &mut process::Child) {
    #[cfg(windows)]
    {
        let pid = child.id();
        if pid != 0 {
            let _ = process::Command::new("taskkill")
                .arg("/F")
                .arg("/T")
                .arg("/PID")
                .arg(pid.to_string())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        let _ = child.kill();
    }

    #[cfg(not(windows))]
    {
        let _ = child.kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{self, Read},
        path::PathBuf,
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
    };

    struct FailingReader;

    impl Read for FailingReader {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "fixture read failure",
            ))
        }
    }

    struct PanickingReader;

    impl Read for PanickingReader {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            panic!("fixture reader panic");
        }
    }

    #[test]
    fn collect_pipe_rejects_reader_io_failures() {
        let error = collect_pipe(spawn_pipe_reader(Some(FailingReader)), "stdout")
            .expect_err("a reader I/O error must not become partial success");
        assert!(matches!(error, DomainError::ExternalTool(_)));
        assert!(error.to_string().contains("stdout"));
    }

    #[test]
    fn collect_pipe_rejects_panicked_reader_threads() {
        let error = collect_pipe(spawn_pipe_reader(Some(PanickingReader)), "stderr")
            .expect_err("a panicked reader must not become an empty stream");
        assert!(matches!(error, DomainError::ExternalTool(_)));
        assert!(error.to_string().contains("stderr"));
    }

    #[test]
    fn reap_pipe_waits_for_reader_thread() {
        let completed = Arc::new(AtomicBool::new(false));
        let reader_completed = Arc::clone(&completed);
        let reader = thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            reader_completed.store(true, Ordering::SeqCst);
            Ok::<Vec<u8>, io::Error>(Vec::new())
        });

        reap_pipe(Some(reader), "stderr");

        assert!(completed.load(Ordering::SeqCst));
    }

    #[test]
    fn validate_command_rejects_empty_program() {
        let err = validate_command("").expect_err("empty command should fail");
        assert!(err.to_string().contains("命令不能为空"));
    }

    fn write_bulk_fixture(tag: &str) -> (PathBuf, usize) {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock should be available")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("nwflash-{tag}-{nonce}.txt"));
        // ~1 MiB, far past any OS pipe buffer (4 KiB - 64 KiB on Windows).
        let mut payload = String::with_capacity(1024 * 1024);
        for index in 0..4096 {
            payload.push_str(&format!("{index:06} {}\n", "x".repeat(249)));
        }
        std::fs::write(&path, &payload).expect("fixture should be written");
        (path, payload.len())
    }

    /// `fastboot getvar all` on a 100+ partition device and `ls -laL` on a large
    /// directory both emit far more than the OS pipe buffer.  The polling loop
    /// must not wait for exit while the child is blocked writing into a pipe
    /// nobody reads — that wedged the single-permit OperationCoordinator and
    /// locked up the whole app until it was killed.
    #[test]
    fn run_command_drains_large_stdout_without_deadlocking() {
        let (path, expected_len) = write_bulk_fixture("pipe-stdout");

        let (sender, receiver) = std::sync::mpsc::channel();
        let probe = path.clone();
        thread::spawn(move || {
            let result = run_command(ProcessCommand {
                program: "cmd".to_string(),
                args: vec![
                    "/C".to_string(),
                    "type".to_string(),
                    probe.to_string_lossy().into_owned(),
                ],
                working_directory: None,
                environment: Vec::new(),
            });
            let _ = sender.send(result);
        });

        let output = receiver
            .recv_timeout(Duration::from_secs(30))
            .expect("large stdout must not deadlock the polling loop")
            .expect("command should run");
        let _ = std::fs::remove_file(&path);

        assert_eq!(output.exit_code, 0);
        assert!(
            output.stdout.len() >= expected_len / 2,
            "expected the full stream, got {} bytes",
            output.stdout.len()
        );
    }

    /// The file-stdout variant redirects stdout to disk but still pipes stderr,
    /// so a chatty child can wedge it the same way.
    #[test]
    fn run_command_with_file_stdout_drains_large_stderr_without_deadlocking() {
        let (path, _) = write_bulk_fixture("pipe-stderr");
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock should be available")
            .as_nanos();
        let sink = std::env::temp_dir().join(format!("nwflash-pipe-sink-{nonce}.bin"));

        let (sender, receiver) = std::sync::mpsc::channel();
        let probe = path.clone();
        let sink_path = sink.clone();
        thread::spawn(move || {
            let result = run_command_with_file_stdout_and_cancel(
                ProcessCommand {
                    program: "cmd".to_string(),
                    args: vec![
                        "/C".to_string(),
                        format!("type {} 1>&2", probe.to_string_lossy()),
                    ],
                    working_directory: None,
                    environment: Vec::new(),
                },
                &sink_path,
                None,
                || false,
            );
            let _ = sender.send(result);
        });

        let output = receiver
            .recv_timeout(Duration::from_secs(30))
            .expect("large stderr must not deadlock the polling loop")
            .expect("command should run");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&sink);

        assert_eq!(output.exit_code, 0);
        assert!(!output.stderr.is_empty());
    }

    #[test]
    fn validate_args_rejects_empty_argument() {
        let err =
            validate_args(&[String::new(), "ok".to_string()]).expect_err("empty arg should fail");
        assert!(err.to_string().contains("参数不能为空"));
    }

    #[test]
    fn validate_working_directory_rejects_missing_path() {
        let missing = PathBuf::from("C:/__nwflash_missing__");
        let err = validate_working_directory(&missing).expect_err("missing path should fail");
        assert!(err.to_string().contains("工作目录不存在"));
    }

    #[test]
    fn run_command_uses_array_args_and_records_exit_code() {
        let output = run_command(ProcessCommand {
            program: "cmd".to_string(),
            args: vec!["/C".to_string(), "echo".to_string(), "ok".to_string()],
            working_directory: None,
            environment: Vec::new(),
        })
        .expect("command should run");

        assert_eq!(output.exit_code, 0);
        assert!(output.stdout.contains("ok"));
    }

    #[test]
    fn run_command_with_env_carries_value() {
        let output = run_command(ProcessCommand {
            program: "cmd".to_string(),
            args: vec![
                "/C".to_string(),
                "echo".to_string(),
                "%NWFLASH_TEST_ENV%".to_string(),
            ],
            working_directory: None,
            environment: vec![("NWFLASH_TEST_ENV".to_string(), "from-test-env".to_string())],
        })
        .expect("command should run");

        assert!(output.stdout.contains("from-test-env"));
    }

    #[test]
    fn run_command_with_timeout_terminates_long_running_process() {
        let error = run_command_with_timeout(
            ProcessCommand {
                program: "cmd".to_string(),
                args: vec![
                    "/C".to_string(),
                    "ping".to_string(),
                    "127.0.0.1".to_string(),
                    "-n".to_string(),
                    "10".to_string(),
                ],
                working_directory: None,
                environment: Vec::new(),
            },
            Some(Duration::from_millis(100)),
        )
        .expect_err("long command should timeout");

        assert!(error.to_string().contains("命令执行超时"));
    }

    #[test]
    fn run_command_with_timeout_reaps_readers_after_large_output() {
        let (path, _) = write_bulk_fixture("pipe-timeout");
        let error = run_command_with_timeout(
            ProcessCommand::new(
                "cmd",
                [
                    "/C".to_string(),
                    format!(
                        "type \"{}\" & ping 127.0.0.1 -n 10 > nul",
                        path.to_string_lossy()
                    ),
                ],
            ),
            Some(Duration::from_millis(100)),
        )
        .expect_err("the delayed child must time out");
        let _ = std::fs::remove_file(path);

        assert!(error.to_string().contains("命令执行超时"));
    }

    #[test]
    fn run_command_with_cancel_stops_process_when_cancelled() {
        let canceled = Arc::new(AtomicBool::new(false));
        let source = Arc::clone(&canceled);
        std::thread::spawn(move || {
            thread::sleep(Duration::from_millis(120));
            source.store(true, Ordering::SeqCst);
        });

        let error = run_command_with_cancel(
            ProcessCommand {
                program: "cmd".to_string(),
                args: vec![
                    "/C".to_string(),
                    "ping".to_string(),
                    "127.0.0.1".to_string(),
                    "-n".to_string(),
                    "10".to_string(),
                ],
                working_directory: None,
                environment: Vec::new(),
            },
            None,
            || canceled.load(Ordering::SeqCst),
        )
        .expect_err("command should stop when cancelled");

        assert!(error.to_string().contains("运行被用户取消"));
    }

    #[test]
    fn run_command_with_cancel_reaps_readers_after_large_output() {
        let (path, _) = write_bulk_fixture("pipe-cancel");
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancel_source = Arc::clone(&cancelled);
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(120));
            cancel_source.store(true, Ordering::SeqCst);
        });

        let error = run_command_with_cancel(
            ProcessCommand::new(
                "cmd",
                [
                    "/C".to_string(),
                    format!(
                        "type \"{}\" & ping 127.0.0.1 -n 10 > nul",
                        path.to_string_lossy()
                    ),
                ],
            ),
            None,
            || cancelled.load(Ordering::SeqCst),
        )
        .expect_err("the delayed child must be cancelled");
        let _ = std::fs::remove_file(path);

        assert!(error.to_string().contains("运行被用户取消"));
    }

    #[test]
    fn run_command_with_cancel_can_return_zero_when_timeout_is_never_set() {
        let immediate_flag = Arc::new(AtomicBool::new(false));
        let output = run_command_with_cancel(
            ProcessCommand {
                program: "cmd".to_string(),
                args: vec!["/C".to_string(), "echo".to_string(), "ok".to_string()],
                working_directory: None,
                environment: Vec::new(),
            },
            None,
            || immediate_flag.load(Ordering::SeqCst),
        )
        .expect("command should run");

        assert_eq!(output.exit_code, 0);
        assert!(output.stdout.contains("ok"));
    }

    #[test]
    fn system_cancellable_executor_preserves_process_output_contract() {
        let executor = SystemCancellableProcessExecutor;
        let output = executor
            .run(
                ProcessCommand::new(
                    "cmd",
                    [
                        "/C".to_string(),
                        "echo".to_string(),
                        "runner-ok".to_string(),
                    ],
                ),
                &mut || false,
            )
            .expect("system executor should run a parameterized process");

        assert_eq!(output.exit_code, 0);
        assert!(output.stdout.contains("runner-ok"));
    }

    #[test]
    fn run_command_with_file_stdin_streams_the_selected_file_to_the_child_process() {
        let input_path = std::env::temp_dir().join("nwflash-process-stdin-test.bin");
        std::fs::write(&input_path, b"partition-image-bytes")
            .expect("fixture input should be written");

        let output = run_command_with_file_stdin_and_cancel(
            ProcessCommand::new("cmd", ["/C".to_string(), "more".to_string()]),
            &input_path,
            None,
            || false,
        )
        .expect("child should receive the fixture bytes on stdin");

        assert_eq!(output.exit_code, 0);
        assert!(output.stdout.contains("partition-image-bytes"));
        std::fs::remove_file(input_path).expect("fixture input should be removed");
    }

    #[test]
    fn run_command_with_file_stdout_streams_child_output_to_the_selected_file() {
        let output_path = std::env::temp_dir().join("nwflash-process-stdout-test.bin");

        let output = run_command_with_file_stdout_and_cancel(
            ProcessCommand::new(
                "cmd",
                [
                    "/C".to_string(),
                    "echo".to_string(),
                    "partition-backup".to_string(),
                ],
            ),
            &output_path,
            None,
            || false,
        )
        .expect("child output should stream to the fixture file");

        assert_eq!(output.exit_code, 0);
        assert_eq!(
            std::fs::read(&output_path).expect("output should be readable"),
            b"partition-backup\r\n"
        );
        std::fs::remove_file(output_path).expect("fixture output should be removed");
    }
}
