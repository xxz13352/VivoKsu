use std::{
    fs::File,
    io::{self, Read},
    path::{Path, PathBuf},
    process::{self, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender},
        Arc, Condvar, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use nwflash_domain::DomainError;

const KILL_POLL_STEP: Duration = Duration::from_millis(10);
pub const PROCESS_OBSERVER_BUFFER_BYTES: usize = 16 * 1024;
pub(crate) const PROCESS_OBSERVER_MAX_LOSSES: usize = 256;
const PROCESS_OBSERVER_QUEUE_CAPACITY: usize = 64;
const PROCESS_OBSERVER_DRAIN_TIMEOUT: Duration = Duration::from_millis(100);
const PROCESS_TERMINATION_GRACE: Duration = Duration::from_secs(2);
const PROCESS_OUTPUT_MAX_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessOutputStream {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessStdinMode {
    Inherit,
    BinaryFile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessStdoutMode {
    TextPipe,
    BinaryFile,
    Unavailable,
}

/// Structured command metadata presented to a trace adapter.
///
/// Deliberately absent: the child environment. Environment values can contain
/// credentials and must never cross the process-observation boundary.
#[derive(Clone, PartialEq, Eq)]
pub struct ProcessStartMetadata {
    pub program: String,
    pub args: Vec<String>,
    pub working_directory: Option<PathBuf>,
    pub stdin_mode: ProcessStdinMode,
    pub stdout_mode: ProcessStdoutMode,
    pub elevated: bool,
}

impl std::fmt::Debug for ProcessStartMetadata {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProcessStartMetadata")
            .field("program", &"[STRUCTURED]")
            .field("args", &format_args!("[{} STRUCTURED]", self.args.len()))
            .field(
                "working_directory",
                &self.working_directory.as_ref().map(|_| "[STRUCTURED]"),
            )
            .field("stdin_mode", &self.stdin_mode)
            .field("stdout_mode", &self.stdout_mode)
            .field("elevated", &self.elevated)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessTermination {
    Completed,
    SpawnFailed,
    WaitFailed,
    OutputReadFailed,
    Cancelled,
    TimedOut,
    TerminationUnconfirmed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessFinishMetadata {
    pub exit_code: Option<i32>,
    pub termination: ProcessTermination,
    /// Whether a best-effort termination command was issued. This is not a
    /// proof that every descendant process exited; `TerminationUnconfirmed`
    /// signals when the helper could not establish termination success.
    pub process_tree_termination_requested: bool,
}

#[derive(Clone, Copy)]
pub enum ProcessObservation<'a> {
    Started(&'a ProcessStartMetadata),
    Output {
        stream: ProcessOutputStream,
        sequence: u64,
        bytes: &'a [u8],
    },
    Finished(ProcessFinishMetadata),
}

/// Observer callbacks are intentionally independent of trace storage, HTTP,
/// authentication and UI state. A higher layer owns redaction and persistence.
pub trait ProcessOutputObserver: Send + Sync {
    fn observe(&self, observation: ProcessObservation<'_>) -> Result<(), ProcessObserverError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessObserverError;

impl std::fmt::Display for ProcessObserverError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("process observer rejected an event")
    }
}

impl std::error::Error for ProcessObserverError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessObserverCallback {
    Started,
    Output,
    Finished,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessObserverLoss {
    pub callback: ProcessObserverCallback,
    pub stream: Option<ProcessOutputStream>,
    pub sequence: Option<u64>,
}

#[must_use = "callers must handle both the command result and observer_losses"]
pub struct ObservedProcessOutcome {
    pub result: Result<ProcessOutput, DomainError>,
    pub observer_losses: Vec<ProcessObserverLoss>,
}

impl std::fmt::Debug for ObservedProcessOutcome {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let result = match &self.result {
            Ok(output) => format!(
                "Ok(exit_code={}, stdout_len={}, stderr_len={})",
                output.exit_code,
                output.stdout.len(),
                output.stderr.len()
            ),
            Err(_) => "Err([REDACTED])".to_string(),
        };
        formatter
            .debug_struct("ObservedProcessOutcome")
            .field("result", &result)
            .field("observer_losses", &self.observer_losses)
            .finish()
    }
}

impl std::fmt::Debug for ProcessOutput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProcessOutput")
            .field("exit_code", &self.exit_code)
            .field("stdout_len", &self.stdout.len())
            .field("stderr_len", &self.stderr.len())
            .finish()
    }
}

/*
 * The observer is fed by a bounded queue so a slow trace/UI sink cannot block
 * a pipe reader and deadlock a chatty child. Queue overflow is an explicit,
 * bounded loss; raw output is never retained by the dispatcher after delivery.
 */
struct ObservationDispatcherState {
    losses: Mutex<Vec<ProcessObserverLoss>>,
    pending: std::sync::atomic::AtomicUsize,
    drained: Condvar,
    drain_lock: Mutex<()>,
}

enum OwnedProcessObservation {
    Started(ProcessStartMetadata),
    Output {
        stream: ProcessOutputStream,
        sequence: u64,
        bytes: Vec<u8>,
    },
    Finished(ProcessFinishMetadata),
}

impl OwnedProcessObservation {
    fn loss(&self) -> ProcessObserverLoss {
        match self {
            Self::Started(_) => ProcessObserverLoss {
                callback: ProcessObserverCallback::Started,
                stream: None,
                sequence: None,
            },
            Self::Output {
                stream, sequence, ..
            } => ProcessObserverLoss {
                callback: ProcessObserverCallback::Output,
                stream: Some(*stream),
                sequence: Some(*sequence),
            },
            Self::Finished(_) => ProcessObserverLoss {
                callback: ProcessObserverCallback::Finished,
                stream: None,
                sequence: None,
            },
        }
    }
}

fn deliver_observation(
    observer: &Arc<dyn ProcessOutputObserver>,
    state: &Arc<ObservationDispatcherState>,
    observation: OwnedProcessObservation,
) {
    let loss = observation.loss();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match &observation {
        OwnedProcessObservation::Started(metadata) => {
            observer.observe(ProcessObservation::Started(metadata))
        }
        OwnedProcessObservation::Output {
            stream,
            sequence,
            bytes,
        } => observer.observe(ProcessObservation::Output {
            stream: *stream,
            sequence: *sequence,
            bytes: bytes.as_slice(),
        }),
        OwnedProcessObservation::Finished(metadata) => {
            observer.observe(ProcessObservation::Finished(*metadata))
        }
    }));
    if !matches!(result, Ok(Ok(()))) {
        record_observer_loss(state, loss);
    }
    state
        .pending
        .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    state.drained.notify_all();
}

impl ObservedProcessOutcome {
    fn into_legacy_result(self) -> Result<ProcessOutput, DomainError> {
        self.result
    }
}

#[derive(Clone)]
pub(crate) struct ObservationDispatcher {
    sender: SyncSender<OwnedProcessObservation>,
    finished_sender: SyncSender<OwnedProcessObservation>,
    state: Arc<ObservationDispatcherState>,
}

impl ObservationDispatcher {
    pub(crate) fn new(observer: Arc<dyn ProcessOutputObserver>) -> Self {
        let (sender, receiver) =
            mpsc::sync_channel::<OwnedProcessObservation>(PROCESS_OBSERVER_QUEUE_CAPACITY);
        let (finished_sender, finished_receiver) = mpsc::sync_channel::<OwnedProcessObservation>(1);
        let state = Arc::new(ObservationDispatcherState {
            losses: Mutex::new(Vec::new()),
            pending: std::sync::atomic::AtomicUsize::new(0),
            drained: Condvar::new(),
            drain_lock: Mutex::new(()),
        });
        let worker_state = Arc::clone(&state);
        thread::Builder::new()
            .name("nwflash-process-observer".to_string())
            .spawn(move || loop {
                let observation = match finished_receiver.try_recv() {
                    Ok(observation) => observation,
                    Err(std::sync::mpsc::TryRecvError::Empty) => {
                        match receiver.recv_timeout(Duration::from_millis(5)) {
                            Ok(observation) => observation,
                            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                        }
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
                };
                deliver_observation(&observer, &worker_state, observation);
            })
            .ok();
        Self {
            sender,
            finished_sender,
            state,
        }
    }

    pub(crate) fn started(&self, metadata: &ProcessStartMetadata) {
        self.dispatch(OwnedProcessObservation::Started(metadata.clone()));
    }

    fn output(&self, stream: ProcessOutputStream, sequence: u64, bytes: &[u8]) {
        self.dispatch(OwnedProcessObservation::Output {
            stream,
            sequence,
            bytes: bytes.to_vec(),
        });
    }

    pub(crate) fn finished(&self, metadata: ProcessFinishMetadata) {
        let observation = OwnedProcessObservation::Finished(metadata);
        let loss = observation.loss();
        self.state
            .pending
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        if self.finished_sender.try_send(observation).is_err() {
            self.state
                .pending
                .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
            record_observer_loss(&self.state, loss);
        }
    }

    fn dispatch(&self, observation: OwnedProcessObservation) {
        let loss = observation.loss();
        self.state
            .pending
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        let mut observation = observation;
        let deadline = Instant::now() + Duration::from_millis(2);
        let mut sent = false;
        loop {
            match self.sender.try_send(observation) {
                Ok(()) => {
                    sent = true;
                    break;
                }
                Err(std::sync::mpsc::TrySendError::Full(next)) => {
                    observation = next;
                    if Instant::now() >= deadline {
                        break;
                    }
                    thread::yield_now();
                }
                Err(std::sync::mpsc::TrySendError::Disconnected(_)) => break,
            }
        }
        if !sent {
            self.state
                .pending
                .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
            record_observer_loss(&self.state, loss);
        }
    }

    pub(crate) fn outcome(
        &self,
        result: Result<ProcessOutput, DomainError>,
    ) -> ObservedProcessOutcome {
        let deadline = Instant::now() + PROCESS_OBSERVER_DRAIN_TIMEOUT;
        let mut guard = self
            .state
            .drain_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while self
            .state
            .pending
            .load(std::sync::atomic::Ordering::Acquire)
            != 0
        {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            let (next_guard, timeout) = self
                .state
                .drained
                .wait_timeout(guard, remaining)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            guard = next_guard;
            if timeout.timed_out() {
                break;
            }
        }
        drop(guard);
        let observer_losses = match self.state.losses.lock() {
            Ok(losses) => losses.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        };
        ObservedProcessOutcome {
            result,
            observer_losses,
        }
    }
}

fn record_observer_loss(state: &ObservationDispatcherState, loss: ProcessObserverLoss) {
    let mut losses = state
        .losses
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if losses.len() < PROCESS_OBSERVER_MAX_LOSSES {
        losses.push(loss);
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct DiscardingProcessObserver;

impl ProcessOutputObserver for DiscardingProcessObserver {
    fn observe(&self, _observation: ProcessObservation<'_>) -> Result<(), ProcessObserverError> {
        Ok(())
    }
}

/// Drains a child pipe on its own thread.
///
/// The supervision loop below only polls `try_wait()`, so nothing else reads
/// these pipes while the child runs.  A child that writes more than the OS pipe
/// buffer (4 KiB - 64 KiB on Windows) would block forever on `write`, `try_wait`
/// would never report an exit, and the single-permit `OperationCoordinator`
/// would stay held — locking every page in the app until the process was killed.
type PipeReader = Option<JoinHandle<io::Result<Vec<u8>>>>;

#[cfg(test)]
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

fn spawn_observed_pipe_reader<R>(
    stream: Option<R>,
    output_stream: ProcessOutputStream,
    dispatcher: ObservationDispatcher,
    output_limit_exceeded: Arc<AtomicBool>,
) -> PipeReader
where
    R: Read + Send + 'static,
{
    stream.map(|mut stream| {
        thread::spawn(move || {
            let mut complete = Vec::new();
            let mut truncated = false;
            let mut sequence = 0_u64;
            let mut buffer = [0_u8; PROCESS_OBSERVER_BUFFER_BYTES];
            loop {
                let read = stream.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                if complete.len() < PROCESS_OUTPUT_MAX_BYTES {
                    let retained = (PROCESS_OUTPUT_MAX_BYTES - complete.len()).min(read);
                    complete.extend_from_slice(&buffer[..retained]);
                    truncated |= retained != read;
                    if retained != read {
                        output_limit_exceeded.store(true, Ordering::Release);
                    }
                } else {
                    truncated = true;
                    output_limit_exceeded.store(true, Ordering::Release);
                }
                if !output_limit_exceeded.load(Ordering::Acquire) {
                    dispatcher.output(output_stream, sequence, &buffer[..read]);
                }
                sequence += 1;
            }
            if truncated {
                Err(io::Error::new(
                    io::ErrorKind::FileTooLarge,
                    "command output exceeded the safety limit",
                ))
            } else {
                Ok(complete)
            }
        })
    })
}

fn collect_pipe(mut reader: PipeReader, stream_label: &str) -> Result<Vec<u8>, DomainError> {
    let Some(handle) = reader.as_ref() else {
        return Ok(Vec::new());
    };
    let deadline = Instant::now() + PROCESS_TERMINATION_GRACE;
    while !handle.is_finished() {
        if Instant::now() >= deadline {
            let _ = reader.take();
            return Err(DomainError::ExternalTool(format!(
                "读取命令 {stream_label} 输出失败：读取超时。"
            )));
        }
        thread::sleep(KILL_POLL_STEP);
    }

    let Some(reader) = reader.take() else {
        return Ok(Vec::new());
    };
    match reader.join() {
        Ok(Ok(bytes)) => Ok(bytes),
        Ok(Err(_)) | Err(_) => Err(DomainError::ExternalTool(format!(
            "读取命令 {stream_label} 输出失败。"
        ))),
    }
}

fn reap_pipe_bounded(reader: &mut PipeReader, stream_label: &str, deadline: Instant) -> bool {
    let Some(handle) = reader.as_ref() else {
        return true;
    };
    if !handle.is_finished() && Instant::now() < deadline {
        return false;
    }
    let reader = reader.take();
    collect_pipe(reader, stream_label).is_ok()
}

/// Best-effort bounded reap after cancellation/timeout. Dropping an unfinished
/// reader detaches it; this is preferable to holding the operation coordinator
/// forever when a descendant inherited the pipe handle.
fn reap_after_termination(
    child: &mut process::Child,
    readers: &mut [(&mut PipeReader, &str)],
) -> bool {
    let deadline = Instant::now() + PROCESS_TERMINATION_GRACE;
    loop {
        let child_done = match child.try_wait() {
            Ok(Some(_)) => true,
            Ok(None) | Err(_) => false,
        };
        let readers_done = readers
            .iter()
            .all(|(reader, _)| reader.as_ref().is_none_or(JoinHandle::is_finished));
        if child_done && readers_done {
            for (reader, label) in readers.iter_mut() {
                let _ = reap_pipe_bounded(reader, label, deadline);
            }
            return true;
        }
        if Instant::now() >= deadline {
            for (reader, label) in readers.iter_mut() {
                if reader.as_ref().is_some_and(JoinHandle::is_finished) {
                    let _ = reap_pipe_bounded(reader, label, deadline);
                } else {
                    let _ = reader.take();
                }
            }
            return false;
        }
        thread::sleep(KILL_POLL_STEP);
    }
}

fn reap_pipe(reader: PipeReader, stream_label: &str) {
    let _ = collect_pipe(reader, stream_label);
}

#[derive(Clone, PartialEq, Eq)]
pub struct ProcessCommand {
    pub program: String,
    pub args: Vec<String>,
    pub working_directory: Option<PathBuf>,
    pub environment: Vec<(String, String)>,
}

impl std::fmt::Debug for ProcessCommand {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProcessCommand")
            .field("program", &"[STRUCTURED]")
            .field("args", &format_args!("[{} STRUCTURED]", self.args.len()))
            .field(
                "working_directory",
                &self.working_directory.as_ref().map(|_| "[STRUCTURED]"),
            )
            .field(
                "environment",
                &format_args!("[{} KEYS]", self.environment.len()),
            )
            .finish()
    }
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

#[derive(Clone, PartialEq, Eq)]
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
    run_command_observed(spec, Arc::new(DiscardingProcessObserver)).into_legacy_result()
}

pub fn run_command_with_timeout(
    spec: ProcessCommand,
    timeout: Option<Duration>,
) -> Result<ProcessOutput, DomainError> {
    run_command_with_cancel_observed(spec, timeout, || false, Arc::new(DiscardingProcessObserver))
        .into_legacy_result()
}

pub fn run_command_with_cancel<F>(
    spec: ProcessCommand,
    timeout: Option<Duration>,
    should_cancel: F,
) -> Result<ProcessOutput, DomainError>
where
    F: FnMut() -> bool,
{
    run_command_with_cancel_observed(
        spec,
        timeout,
        should_cancel,
        Arc::new(DiscardingProcessObserver),
    )
    .into_legacy_result()
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
    run_command_with_file_stdin_and_cancel_observed(
        spec,
        input_path,
        timeout,
        should_cancel,
        Arc::new(DiscardingProcessObserver),
    )
    .into_legacy_result()
}

pub fn run_command_observed(
    spec: ProcessCommand,
    observer: Arc<dyn ProcessOutputObserver>,
) -> ObservedProcessOutcome {
    run_command_with_cancel_observed(spec, None, || false, observer)
}

pub fn run_command_with_cancel_observed<F>(
    spec: ProcessCommand,
    timeout: Option<Duration>,
    should_cancel: F,
    observer: Arc<dyn ProcessOutputObserver>,
) -> ObservedProcessOutcome
where
    F: FnMut() -> bool,
{
    run_command_with_optional_file_stdin_observed(
        spec,
        None,
        ProcessStdinMode::Inherit,
        timeout,
        should_cancel,
        observer,
    )
}

pub fn run_command_with_file_stdin_and_cancel_observed<F>(
    spec: ProcessCommand,
    input_path: &std::path::Path,
    timeout: Option<Duration>,
    should_cancel: F,
    observer: Arc<dyn ProcessOutputObserver>,
) -> ObservedProcessOutcome
where
    F: FnMut() -> bool,
{
    let input = File::open(input_path)
        .map_err(|error| DomainError::InvalidInput(format!("无法打开命令输入文件：{error}")));
    match input {
        Ok(input) => run_command_with_optional_file_stdin_observed(
            spec,
            Some(input),
            ProcessStdinMode::BinaryFile,
            timeout,
            should_cancel,
            observer,
        ),
        Err(error) => ObservedProcessOutcome {
            result: Err(error),
            observer_losses: Vec::new(),
        },
    }
}

pub fn run_command_with_file_stdout_and_cancel<F>(
    spec: ProcessCommand,
    output_path: &std::path::Path,
    timeout: Option<Duration>,
    should_cancel: F,
) -> Result<ProcessOutput, DomainError>
where
    F: FnMut() -> bool,
{
    run_command_with_file_stdout_and_cancel_observed(
        spec,
        output_path,
        timeout,
        should_cancel,
        Arc::new(DiscardingProcessObserver),
    )
    .into_legacy_result()
}

pub fn run_command_with_file_stdout_and_cancel_observed<F>(
    spec: ProcessCommand,
    output_path: &std::path::Path,
    timeout: Option<Duration>,
    mut should_cancel: F,
    observer: Arc<dyn ProcessOutputObserver>,
) -> ObservedProcessOutcome
where
    F: FnMut() -> bool,
{
    let dispatcher = ObservationDispatcher::new(observer);
    let result = run_command_with_file_stdout_and_cancel_observed_core(
        spec,
        output_path,
        timeout,
        &mut should_cancel,
        &dispatcher,
    );
    dispatcher.outcome(result)
}

fn run_command_with_file_stdout_and_cancel_observed_core(
    spec: ProcessCommand,
    output_path: &std::path::Path,
    timeout: Option<Duration>,
    should_cancel: &mut dyn FnMut() -> bool,
    dispatcher: &ObservationDispatcher,
) -> Result<ProcessOutput, DomainError> {
    let program = validate_command(&spec.program)?;
    validate_args(&spec.args)?;
    validate_environment(&spec.environment)?;
    if let Some(work_dir) = &spec.working_directory {
        validate_working_directory(work_dir)?;
    }
    let output_file = File::create(output_path)
        .map_err(|error| DomainError::InvalidInput(format!("无法创建命令输出文件：{error}")))?;

    let start_metadata = ProcessStartMetadata {
        program: program.to_string(),
        args: spec.args.clone(),
        working_directory: spec.working_directory.clone(),
        stdin_mode: ProcessStdinMode::Inherit,
        stdout_mode: ProcessStdoutMode::BinaryFile,
        elevated: false,
    };
    dispatcher.started(&start_metadata);

    let mut command = process::Command::new(program);
    command.args(&spec.args);
    if let Some(work_dir) = &spec.working_directory {
        command.current_dir(work_dir);
    }
    command.envs(spec.environment);
    command.stdout(Stdio::from(output_file));
    command.stderr(Stdio::piped());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            dispatcher.finished(ProcessFinishMetadata {
                exit_code: None,
                termination: ProcessTermination::SpawnFailed,
                process_tree_termination_requested: false,
            });
            return Err(DomainError::ExternalTool(format!("执行命令失败: {error}")));
        }
    };

    // stdout already goes straight to `output_path`; stderr is still a pipe and
    // must be drained concurrently or a chatty child wedges the loop below.
    let output_limit_exceeded = Arc::new(AtomicBool::new(false));
    let mut stderr_reader = spawn_observed_pipe_reader(
        child.stderr.take(),
        ProcessOutputStream::Stderr,
        dispatcher.clone(),
        Arc::clone(&output_limit_exceeded),
    );

    let start = Instant::now();
    loop {
        let exited = match child.try_wait() {
            Ok(status) => status.is_some(),
            Err(error) => {
                let _ = terminate_process_tree(&mut child);
                let terminated =
                    reap_after_termination(&mut child, &mut [(&mut stderr_reader, "stderr")]);
                dispatcher.finished(ProcessFinishMetadata {
                    exit_code: None,
                    termination: if terminated {
                        ProcessTermination::WaitFailed
                    } else {
                        ProcessTermination::TerminationUnconfirmed
                    },
                    process_tree_termination_requested: true,
                });
                return Err(DomainError::ExternalTool(format!(
                    "等待命令结束失败: {error}"
                )));
            }
        };
        if exited {
            break;
        }
        if output_limit_exceeded.load(Ordering::Acquire) {
            let _ = terminate_process_tree(&mut child);
            let _terminated =
                reap_after_termination(&mut child, &mut [(&mut stderr_reader, "stderr")]);
            dispatcher.finished(ProcessFinishMetadata {
                exit_code: None,
                termination: ProcessTermination::TerminationUnconfirmed,
                process_tree_termination_requested: true,
            });
            return Err(DomainError::ExternalTool(
                "命令输出超过安全上限，已请求终止但子进程树状态未确认。".to_string(),
            ));
        }
        if should_cancel() {
            let _ = terminate_process_tree(&mut child);
            let _terminated =
                reap_after_termination(&mut child, &mut [(&mut stderr_reader, "stderr")]);
            dispatcher.finished(ProcessFinishMetadata {
                exit_code: None,
                termination: ProcessTermination::TerminationUnconfirmed,
                process_tree_termination_requested: true,
            });
            return Err(DomainError::UserCancelled("运行被用户取消".to_string()));
        }
        if timeout.is_some_and(|limit| start.elapsed() >= limit) {
            let _ = terminate_process_tree(&mut child);
            let _terminated =
                reap_after_termination(&mut child, &mut [(&mut stderr_reader, "stderr")]);
            dispatcher.finished(ProcessFinishMetadata {
                exit_code: None,
                termination: ProcessTermination::TerminationUnconfirmed,
                process_tree_termination_requested: true,
            });
            return Err(DomainError::ExternalTool(
                "命令执行超时，已请求终止但子进程树状态未确认。".to_string(),
            ));
        }
        thread::sleep(KILL_POLL_STEP);
    }

    let status = match child.wait() {
        Ok(status) => status,
        Err(error) => {
            reap_pipe(stderr_reader, "stderr");
            dispatcher.finished(ProcessFinishMetadata {
                exit_code: None,
                termination: ProcessTermination::WaitFailed,
                process_tree_termination_requested: false,
            });
            return Err(DomainError::ExternalTool(format!("执行命令失败: {error}")));
        }
    };
    let stderr = match collect_pipe(stderr_reader, "stderr") {
        Ok(stderr) => stderr,
        Err(error) => {
            dispatcher.finished(ProcessFinishMetadata {
                exit_code: status.code(),
                termination: ProcessTermination::OutputReadFailed,
                process_tree_termination_requested: false,
            });
            return Err(error);
        }
    };
    let output = ProcessOutput {
        exit_code: status.code().unwrap_or(-1),
        stdout: String::new(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    };
    dispatcher.finished(ProcessFinishMetadata {
        exit_code: status.code(),
        termination: ProcessTermination::Completed,
        process_tree_termination_requested: false,
    });
    Ok(output)
}

fn run_command_with_optional_file_stdin_observed<F>(
    spec: ProcessCommand,
    input: Option<File>,
    stdin_mode: ProcessStdinMode,
    timeout: Option<Duration>,
    mut should_cancel: F,
    observer: Arc<dyn ProcessOutputObserver>,
) -> ObservedProcessOutcome
where
    F: FnMut() -> bool,
{
    let dispatcher = ObservationDispatcher::new(observer);
    let result = run_command_with_optional_file_stdin_observed_core(
        spec,
        input,
        stdin_mode,
        timeout,
        &mut should_cancel,
        &dispatcher,
    );
    dispatcher.outcome(result)
}

fn run_command_with_optional_file_stdin_observed_core(
    spec: ProcessCommand,
    input: Option<File>,
    stdin_mode: ProcessStdinMode,
    timeout: Option<Duration>,
    should_cancel: &mut dyn FnMut() -> bool,
    dispatcher: &ObservationDispatcher,
) -> Result<ProcessOutput, DomainError> {
    let program = validate_command(&spec.program)?;
    validate_args(&spec.args)?;
    validate_environment(&spec.environment)?;

    if let Some(work_dir) = &spec.working_directory {
        validate_working_directory(work_dir)?;
    }

    let start_metadata = ProcessStartMetadata {
        program: program.to_string(),
        args: spec.args.clone(),
        working_directory: spec.working_directory.clone(),
        stdin_mode,
        stdout_mode: ProcessStdoutMode::TextPipe,
        elevated: false,
    };
    dispatcher.started(&start_metadata);

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

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            dispatcher.finished(ProcessFinishMetadata {
                exit_code: None,
                termination: ProcessTermination::SpawnFailed,
                process_tree_termination_requested: false,
            });
            return Err(DomainError::ExternalTool(format!("执行命令失败: {error}")));
        }
    };

    // Both pipes must be drained while the child runs, not after it exits.
    let output_limit_exceeded = Arc::new(AtomicBool::new(false));
    let mut stdout_reader = spawn_observed_pipe_reader(
        child.stdout.take(),
        ProcessOutputStream::Stdout,
        dispatcher.clone(),
        Arc::clone(&output_limit_exceeded),
    );
    let mut stderr_reader = spawn_observed_pipe_reader(
        child.stderr.take(),
        ProcessOutputStream::Stderr,
        dispatcher.clone(),
        Arc::clone(&output_limit_exceeded),
    );

    let start = Instant::now();
    loop {
        let exited = match child.try_wait() {
            Ok(status) => status.is_some(),
            Err(error) => {
                let _ = terminate_process_tree(&mut child);
                let terminated = reap_after_termination(
                    &mut child,
                    &mut [
                        (&mut stdout_reader, "stdout"),
                        (&mut stderr_reader, "stderr"),
                    ],
                );
                dispatcher.finished(ProcessFinishMetadata {
                    exit_code: None,
                    termination: if terminated {
                        ProcessTermination::WaitFailed
                    } else {
                        ProcessTermination::TerminationUnconfirmed
                    },
                    process_tree_termination_requested: true,
                });
                return Err(DomainError::ExternalTool(format!(
                    "等待命令结束失败: {error}"
                )));
            }
        };
        if exited {
            break;
        }

        if output_limit_exceeded.load(Ordering::Acquire) {
            let _ = terminate_process_tree(&mut child);
            let _terminated = reap_after_termination(
                &mut child,
                &mut [
                    (&mut stdout_reader, "stdout"),
                    (&mut stderr_reader, "stderr"),
                ],
            );
            dispatcher.finished(ProcessFinishMetadata {
                exit_code: None,
                termination: ProcessTermination::TerminationUnconfirmed,
                process_tree_termination_requested: true,
            });
            return Err(DomainError::ExternalTool(
                "命令输出超过安全上限，已请求终止但子进程树状态未确认。".to_string(),
            ));
        }

        if should_cancel() {
            let _ = terminate_process_tree(&mut child);
            let _terminated = reap_after_termination(
                &mut child,
                &mut [
                    (&mut stdout_reader, "stdout"),
                    (&mut stderr_reader, "stderr"),
                ],
            );
            dispatcher.finished(ProcessFinishMetadata {
                exit_code: None,
                termination: ProcessTermination::TerminationUnconfirmed,
                process_tree_termination_requested: true,
            });
            return Err(DomainError::UserCancelled("运行被用户取消".to_string()));
        }

        if let Some(timeout) = timeout {
            if start.elapsed() >= timeout {
                let _ = terminate_process_tree(&mut child);
                let _terminated = reap_after_termination(
                    &mut child,
                    &mut [
                        (&mut stdout_reader, "stdout"),
                        (&mut stderr_reader, "stderr"),
                    ],
                );
                dispatcher.finished(ProcessFinishMetadata {
                    exit_code: None,
                    termination: ProcessTermination::TerminationUnconfirmed,
                    process_tree_termination_requested: true,
                });
                return Err(DomainError::ExternalTool(
                    "命令执行超时，已请求终止但子进程树状态未确认。".to_string(),
                ));
            }
        }

        thread::sleep(KILL_POLL_STEP);
    }

    let status = match child.wait() {
        Ok(status) => status,
        Err(error) => {
            reap_pipe(stdout_reader, "stdout");
            reap_pipe(stderr_reader, "stderr");
            dispatcher.finished(ProcessFinishMetadata {
                exit_code: None,
                termination: ProcessTermination::WaitFailed,
                process_tree_termination_requested: false,
            });
            return Err(DomainError::ExternalTool(format!("执行命令失败: {error}")));
        }
    };

    let stdout_result = collect_pipe(stdout_reader, "stdout");
    let stderr_result = collect_pipe(stderr_reader, "stderr");
    let stdout = match stdout_result {
        Ok(stdout) => stdout,
        Err(error) => {
            dispatcher.finished(ProcessFinishMetadata {
                exit_code: status.code(),
                termination: ProcessTermination::OutputReadFailed,
                process_tree_termination_requested: false,
            });
            return Err(error);
        }
    };
    let stderr = match stderr_result {
        Ok(stderr) => stderr,
        Err(error) => {
            dispatcher.finished(ProcessFinishMetadata {
                exit_code: status.code(),
                termination: ProcessTermination::OutputReadFailed,
                process_tree_termination_requested: false,
            });
            return Err(error);
        }
    };
    let exit_code = status.code().unwrap_or(-1);
    let output = ProcessOutput {
        exit_code,
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    };
    dispatcher.finished(ProcessFinishMetadata {
        exit_code: status.code(),
        termination: ProcessTermination::Completed,
        process_tree_termination_requested: false,
    });
    Ok(output)
}

fn terminate_process_tree(child: &mut process::Child) -> bool {
    #[cfg(windows)]
    {
        let pid = child.id();
        if pid != 0 {
            let taskkill = std::path::Path::new(r"C:\Windows\System32\taskkill.exe");
            let tree_killed = process::Command::new(taskkill)
                .arg("/F")
                .arg("/T")
                .arg("/PID")
                .arg(pid.to_string())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|status| status.success());
            let child_killed =
                child.kill().is_ok() || child.try_wait().is_ok_and(|status| status.is_some());
            return tree_killed && child_killed;
        }
        false
    }

    #[cfg(not(windows))]
    {
        child.kill().is_ok() || child.try_wait().is_ok_and(|status| status.is_some())
    }
}

#[cfg(test)]
fn production_source(source: &str) -> &str {
    let mut search_from = 0;
    while let Some(relative) = source[search_from..].find("#[cfg(test)]") {
        let index = search_from + relative + "#[cfg(test)]".len();
        let rest = source[index..].trim_start();
        if rest.starts_with("mod ") || rest.starts_with("pub mod ") {
            return &source[..search_from + relative];
        }
        search_from = index;
    }
    source
}

#[cfg(test)]
fn collect_production_spawn_sites(crates_root: &Path) -> std::collections::BTreeMap<String, usize> {
    fn visit(root: &Path, directory: &Path, sites: &mut std::collections::BTreeMap<String, usize>) {
        let Ok(entries) = std::fs::read_dir(directory) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                visit(root, &path, sites);
                continue;
            }
            let Ok(relative_path) = path.strip_prefix(root) else {
                continue;
            };
            if path.extension().is_none_or(|extension| extension != "rs")
                || !relative_path
                    .components()
                    .any(|component| component.as_os_str() == "src")
            {
                continue;
            }
            let Ok(source) = std::fs::read_to_string(&path) else {
                continue;
            };
            let production = production_source(&source);
            let count = production
                .lines()
                .filter(|line| {
                    !line.contains("line.contains(")
                        && ((line.contains("Command::new(")
                            && !line.contains("ProcessCommand::new("))
                            || line.contains("ShellExecuteExW("))
                })
                .count();
            if count != 0 {
                let relative = relative_path.to_string_lossy().replace('\\', "/");
                sites.insert(relative, count);
            }
        }
    }

    let mut sites = std::collections::BTreeMap::new();
    visit(crates_root, crates_root, &mut sites);
    sites
}

#[cfg(test)]
fn production_spawn_site_allowlist() -> std::collections::BTreeMap<String, usize> {
    [
        ("nwflash-tauri/src/commands/mirror.rs".to_string(), 2),
        ("nwflash-windows/src/driver.rs".to_string(), 1),
        ("nwflash-windows/src/process.rs".to_string(), 3),
    ]
    .into_iter()
    .collect()
}

#[cfg(test)]
fn collect_unobserved_process_callers(
    crates_root: &Path,
) -> std::collections::BTreeMap<String, usize> {
    const LEGACY_CALLS: [&str; 6] = [
        "run_command(",
        "run_command_with_timeout(",
        "run_command_with_cancel(",
        "run_command_with_file_stdin_and_cancel(",
        "run_command_with_file_stdout_and_cancel(",
        "run_elevated_process(",
    ];

    fn visit(
        root: &Path,
        directory: &Path,
        callers: &mut std::collections::BTreeMap<String, usize>,
    ) {
        let Ok(entries) = std::fs::read_dir(directory) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                visit(root, &path, callers);
                continue;
            }
            let Ok(relative_path) = path.strip_prefix(root) else {
                continue;
            };
            let relative = relative_path.to_string_lossy().replace('\\', "/");
            if path.extension().is_none_or(|extension| extension != "rs")
                || !relative_path
                    .components()
                    .any(|component| component.as_os_str() == "src")
                || relative == "nwflash-windows/src/process.rs"
            {
                continue;
            }
            let Ok(source) = std::fs::read_to_string(&path) else {
                continue;
            };
            let production = production_source(&source);
            let count = production
                .lines()
                .filter(|line| {
                    let trimmed = line.trim_start();
                    !trimmed.starts_with("pub fn ")
                        && !trimmed.starts_with("fn ")
                        && LEGACY_CALLS.iter().any(|needle| line.contains(needle))
                })
                .count();
            if count != 0 {
                callers.insert(relative, count);
            }
        }
    }

    let mut callers = std::collections::BTreeMap::new();
    visit(crates_root, crates_root, &mut callers);
    callers
}

#[cfg(test)]
fn unobserved_process_caller_allowlist() -> std::collections::BTreeMap<String, usize> {
    [
        ("nwflash-application/src/firmware_extract.rs".to_string(), 2),
        ("nwflash-tauri/src/commands/device.rs".to_string(), 1),
        (
            "nwflash-tauri/src/commands/device_identity.rs".to_string(),
            1,
        ),
        ("nwflash-tauri/src/commands/files.rs".to_string(), 2),
        ("nwflash-tauri/src/commands/partitions.rs".to_string(), 4),
        ("nwflash-tauri/src/commands/quick_flash.rs".to_string(), 6),
        ("nwflash-tauri/src/commands/root.rs".to_string(), 3),
        ("nwflash-windows/src/driver.rs".to_string(), 1),
        ("nwflash-windows/src/platform_tools.rs".to_string(), 1),
    ]
    .into_iter()
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{self, Cursor, Read},
        path::PathBuf,
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc, Mutex,
        },
    };

    #[derive(Clone, Default)]
    struct RecordingObserver {
        events: Arc<Mutex<Vec<RecordedObservation>>>,
        fail_first_stdout: Arc<AtomicBool>,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum RecordedObservation {
        Started(ProcessStartMetadata),
        Output {
            stream: ProcessOutputStream,
            sequence: u64,
            bytes: Vec<u8>,
        },
        Finished(ProcessFinishMetadata),
    }

    impl RecordingObserver {
        fn failing_first_stdout() -> Self {
            Self {
                events: Arc::default(),
                fail_first_stdout: Arc::new(AtomicBool::new(true)),
            }
        }

        fn snapshot(&self) -> Vec<RecordedObservation> {
            self.events.lock().expect("event lock should hold").clone()
        }
    }

    impl ProcessOutputObserver for RecordingObserver {
        fn observe(&self, observation: ProcessObservation<'_>) -> Result<(), ProcessObserverError> {
            let recorded = match observation {
                ProcessObservation::Started(metadata) => {
                    RecordedObservation::Started(metadata.clone())
                }
                ProcessObservation::Output {
                    stream,
                    sequence,
                    bytes,
                } => {
                    if stream == ProcessOutputStream::Stdout
                        && self.fail_first_stdout.swap(false, Ordering::SeqCst)
                    {
                        return Err(ProcessObserverError);
                    }
                    RecordedObservation::Output {
                        stream,
                        sequence,
                        bytes: bytes.to_vec(),
                    }
                }
                ProcessObservation::Finished(metadata) => RecordedObservation::Finished(metadata),
            };
            self.events
                .lock()
                .expect("event lock should hold")
                .push(recorded);
            Ok(())
        }
    }

    #[derive(Clone, Default)]
    struct BlockingOutputObserver {
        entered: Arc<AtomicBool>,
        release: Arc<AtomicBool>,
    }

    impl ProcessOutputObserver for BlockingOutputObserver {
        fn observe(&self, observation: ProcessObservation<'_>) -> Result<(), ProcessObserverError> {
            if matches!(observation, ProcessObservation::Output { .. }) {
                self.entered.store(true, Ordering::SeqCst);
                while !self.release.load(Ordering::SeqCst) {
                    thread::sleep(Duration::from_millis(1));
                }
            }
            Ok(())
        }
    }

    #[derive(Clone, Default)]
    struct PriorityObserver {
        entered: Arc<AtomicBool>,
        release: Arc<AtomicBool>,
        finished: Arc<AtomicBool>,
    }

    impl ProcessOutputObserver for PriorityObserver {
        fn observe(&self, observation: ProcessObservation<'_>) -> Result<(), ProcessObserverError> {
            match observation {
                ProcessObservation::Output { .. } if !self.entered.swap(true, Ordering::SeqCst) => {
                    while !self.release.load(Ordering::SeqCst) {
                        thread::sleep(Duration::from_millis(1));
                    }
                }
                ProcessObservation::Finished(_) => {
                    self.finished.store(true, Ordering::SeqCst);
                }
                _ => {}
            }
            Ok(())
        }
    }

    #[test]
    fn slow_observer_does_not_block_pipe_reader_or_process_completion() {
        let (path, _) = write_bulk_fixture("slow-observer");
        let observer = BlockingOutputObserver::default();
        let entered = Arc::clone(&observer.entered);
        let release = Arc::clone(&observer.release);
        let (sender, receiver) = std::sync::mpsc::channel();
        let probe = path.clone();
        thread::spawn(move || {
            let outcome = run_command_observed(
                ProcessCommand::new(
                    "cmd",
                    [
                        "/C".to_string(),
                        "type".to_string(),
                        probe.to_string_lossy().into_owned(),
                    ],
                ),
                Arc::new(observer),
            );
            let _ = sender.send(outcome);
        });

        let started = Instant::now();
        while !entered.load(Ordering::SeqCst) && started.elapsed() < Duration::from_secs(2) {
            thread::sleep(Duration::from_millis(1));
        }
        assert!(entered.load(Ordering::SeqCst), "observer should see output");
        let outcome = receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("a slow observer must not hold the pipe reader");
        release.store(true, Ordering::SeqCst);
        let _ = std::fs::remove_file(path);
        assert!(outcome.result.is_ok());
    }

    #[test]
    fn observer_losses_are_bounded() {
        let dispatcher = ObservationDispatcher::new(Arc::new(RecordingObserver::default()));
        for sequence in 0..(PROCESS_OBSERVER_MAX_LOSSES as u64 * 4) {
            dispatcher.dispatch(OwnedProcessObservation::Output {
                stream: ProcessOutputStream::Stdout,
                sequence,
                bytes: b"output".to_vec(),
            });
        }
        let outcome = dispatcher.outcome(Ok(ProcessOutput {
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
        }));
        assert!(outcome.observer_losses.len() <= PROCESS_OBSERVER_MAX_LOSSES);
    }

    #[test]
    fn finished_observation_has_priority_over_full_output_queue() {
        let observer = PriorityObserver::default();
        let entered = Arc::clone(&observer.entered);
        let release = Arc::clone(&observer.release);
        let finished = Arc::clone(&observer.finished);
        let dispatcher = ObservationDispatcher::new(Arc::new(observer));
        dispatcher.output(ProcessOutputStream::Stdout, 0, b"output");
        let started = Instant::now();
        while !entered.load(Ordering::SeqCst) && started.elapsed() < Duration::from_secs(1) {
            thread::sleep(Duration::from_millis(1));
        }
        assert!(entered.load(Ordering::SeqCst));
        for sequence in 1..(PROCESS_OBSERVER_QUEUE_CAPACITY as u64 * 2) {
            dispatcher.output(ProcessOutputStream::Stdout, sequence, b"output");
        }
        dispatcher.finished(ProcessFinishMetadata {
            exit_code: Some(0),
            termination: ProcessTermination::Completed,
            process_tree_termination_requested: false,
        });
        release.store(true, Ordering::SeqCst);
        let _ = dispatcher.outcome(Ok(ProcessOutput {
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
        }));
        assert!(
            finished.load(Ordering::SeqCst),
            "terminal observation must not be dropped"
        );
    }

    #[test]
    fn observed_pipe_reader_rejects_output_above_safety_limit() {
        let oversized = vec![b'x'; PROCESS_OUTPUT_MAX_BYTES + 1];
        let dispatcher = ObservationDispatcher::new(Arc::new(RecordingObserver::default()));
        let limit = Arc::new(AtomicBool::new(false));
        let reader = spawn_observed_pipe_reader(
            Some(Cursor::new(oversized)),
            ProcessOutputStream::Stdout,
            dispatcher,
            limit,
        );
        let error = collect_pipe(reader, "stdout").expect_err("oversized output must be rejected");
        assert!(error.to_string().contains("stdout"));
    }

    #[test]
    fn observed_outcome_debug_never_contains_process_output() {
        let outcome = ObservedProcessOutcome {
            result: Ok(ProcessOutput {
                exit_code: 1,
                stdout: "bearer-secret".to_string(),
                stderr: "cookie-secret".to_string(),
            }),
            observer_losses: Vec::new(),
        };
        let debug = format!("{outcome:?}");
        assert!(!debug.contains("bearer-secret"));
        assert!(!debug.contains("cookie-secret"));
    }

    #[test]
    fn observed_command_start_is_structured_and_never_contains_environment() {
        let observer = RecordingObserver::default();
        let outcome = run_command_observed(
            ProcessCommand::new(
                "cmd",
                ["/C".to_string(), "echo".to_string(), "ok".to_string()],
            )
            .with_env(vec![(
                "NWFLASH_OBSERVER_SECRET".to_string(),
                "must-not-be-observed".to_string(),
            )]),
            Arc::new(observer.clone()),
        );

        outcome.result.expect("fixture command should complete");
        let started = observer
            .snapshot()
            .into_iter()
            .find_map(|event| match event {
                RecordedObservation::Started(metadata) => Some(metadata),
                _ => None,
            })
            .expect("start metadata must be observed");
        assert_eq!(started.program, "cmd");
        assert_eq!(started.args, ["/C", "echo", "ok"]);
        assert_eq!(started.working_directory, None);
        assert_eq!(started.stdout_mode, ProcessStdoutMode::TextPipe);
        assert!(!format!("{started:?}").contains("must-not-be-observed"));
        assert!(!format!("{started:?}").contains("NWFLASH_OBSERVER_SECRET"));
        let command = ProcessCommand::new("cmd", ["--token=must-not-leak".to_string()])
            .with_env(vec![("SECRET".to_string(), "must-not-leak".to_string())]);
        let command_debug = format!("{command:?}");
        assert!(!command_debug.contains("must-not-leak"));
    }

    #[test]
    fn observed_stdout_chunks_are_fixed_size_and_keep_stream_order() {
        let (path, expected_len) = write_bulk_fixture("observed-order");
        let observer = RecordingObserver::default();
        let outcome = run_command_observed(
            ProcessCommand::new(
                "cmd",
                [
                    "/C".to_string(),
                    "type".to_string(),
                    path.to_string_lossy().into_owned(),
                ],
            ),
            Arc::new(observer.clone()),
        );
        let output = outcome.result.expect("fixture command should complete");
        let _ = std::fs::remove_file(path);

        let chunks = observer
            .snapshot()
            .into_iter()
            .filter_map(|event| match event {
                RecordedObservation::Output {
                    stream: ProcessOutputStream::Stdout,
                    sequence,
                    bytes,
                } => Some((sequence, bytes)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(chunks.len() > 1, "fixture must cross an observer buffer");
        assert!(chunks
            .iter()
            .all(|(_, bytes)| bytes.len() <= PROCESS_OBSERVER_BUFFER_BYTES));
        assert_eq!(
            chunks
                .iter()
                .map(|(sequence, _)| *sequence)
                .collect::<Vec<_>>(),
            (0..chunks.len() as u64).collect::<Vec<_>>()
        );
        let observed = chunks
            .into_iter()
            .flat_map(|(_, bytes)| bytes)
            .collect::<Vec<_>>();
        assert_eq!(observed, output.stdout.as_bytes());
        assert!(observed.len() >= expected_len / 2);
    }

    #[test]
    fn observer_failure_reports_loss_without_stopping_pipe_drain() {
        let (path, expected_len) = write_bulk_fixture("observer-loss");
        let observer = RecordingObserver::failing_first_stdout();
        let outcome = run_command_observed(
            ProcessCommand::new(
                "cmd",
                [
                    "/C".to_string(),
                    "type".to_string(),
                    path.to_string_lossy().into_owned(),
                ],
            ),
            Arc::new(observer),
        );
        let output = outcome
            .result
            .expect("observer failure must not fail command");
        let _ = std::fs::remove_file(path);

        assert!(output.stdout.len() >= expected_len / 2);
        assert_eq!(outcome.observer_losses.len(), 1);
        assert_eq!(
            outcome.observer_losses[0],
            ProcessObserverLoss {
                callback: ProcessObserverCallback::Output,
                stream: Some(ProcessOutputStream::Stdout),
                sequence: Some(0),
            }
        );
    }

    #[test]
    fn file_stdout_is_never_emitted_as_text_observation() {
        let output_path = std::env::temp_dir().join("nwflash-observer-binary-output.bin");
        let observer = RecordingObserver::default();
        let outcome = run_command_with_file_stdout_and_cancel_observed(
            ProcessCommand::new(
                "cmd",
                [
                    "/C".to_string(),
                    "echo binary-output & echo diagnostic 1>&2".to_string(),
                ],
            ),
            &output_path,
            None,
            || false,
            Arc::new(observer.clone()),
        );
        outcome.result.expect("fixture command should complete");
        let _ = std::fs::remove_file(output_path);

        let events = observer.snapshot();
        assert!(events.iter().any(|event| matches!(
            event,
            RecordedObservation::Started(ProcessStartMetadata {
                stdout_mode: ProcessStdoutMode::BinaryFile,
                ..
            })
        )));
        assert!(!events.iter().any(|event| matches!(
            event,
            RecordedObservation::Output {
                stream: ProcessOutputStream::Stdout,
                ..
            }
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            RecordedObservation::Output {
                stream: ProcessOutputStream::Stderr,
                ..
            }
        )));
    }

    #[test]
    fn timeout_observation_reports_process_tree_termination() {
        let observer = RecordingObserver::default();
        let outcome = run_command_with_cancel_observed(
            ProcessCommand::new(
                "cmd",
                ["/C".to_string(), "ping 127.0.0.1 -n 10 > nul".to_string()],
            ),
            Some(Duration::from_millis(100)),
            || false,
            Arc::new(observer.clone()),
        );
        assert!(outcome
            .result
            .expect_err("fixture must time out")
            .to_string()
            .contains("超时"));
        assert!(observer.snapshot().iter().any(|event| matches!(
            event,
            RecordedObservation::Finished(ProcessFinishMetadata {
                termination: ProcessTermination::TerminationUnconfirmed,
                process_tree_termination_requested: true,
                ..
            })
        )));
    }

    #[test]
    fn cancellation_observation_reports_process_tree_termination() {
        let observer = RecordingObserver::default();
        let outcome = run_command_with_cancel_observed(
            ProcessCommand::new(
                "cmd",
                ["/C".to_string(), "ping 127.0.0.1 -n 10 > nul".to_string()],
            ),
            None,
            || true,
            Arc::new(observer.clone()),
        );
        assert!(outcome
            .result
            .expect_err("fixture must cancel")
            .to_string()
            .contains("取消"));
        assert!(observer.snapshot().iter().any(|event| matches!(
            event,
            RecordedObservation::Finished(ProcessFinishMetadata {
                termination: ProcessTermination::TerminationUnconfirmed,
                process_tree_termination_requested: true,
                ..
            })
        )));
    }

    #[test]
    fn production_process_spawn_sites_match_the_static_allowlist() {
        let workspace_crates = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crates directory should exist");
        let actual = collect_production_spawn_sites(workspace_crates);
        assert_eq!(actual, production_spawn_site_allowlist());
    }

    #[test]
    fn legacy_unobserved_process_callers_match_the_static_allowlist() {
        let workspace_crates = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crates directory should exist");
        let actual = collect_unobserved_process_callers(workspace_crates);
        assert_eq!(actual, unobserved_process_caller_allowlist());
    }

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

    struct NeverEndingReader;

    impl Read for NeverEndingReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            buffer[0] = b'x';
            thread::sleep(Duration::from_millis(25));
            Ok(1)
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
    fn collect_pipe_detaches_reader_after_bounded_timeout() {
        let started = Instant::now();
        let error = collect_pipe(spawn_pipe_reader(Some(NeverEndingReader)), "stdout")
            .expect_err("an unending reader must not block collection forever");
        assert!(started.elapsed() < PROCESS_TERMINATION_GRACE + Duration::from_secs(1));
        assert!(error.to_string().contains("stdout"));
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
        let script = path.with_extension("cmd");
        std::fs::write(
            &script,
            format!(
                "@echo off\r\ntype \"{}\"\r\nping 127.0.0.1 -n 10 > nul\r\n",
                path.to_string_lossy()
            ),
        )
        .expect("timeout script should be written");
        let error = run_command_with_timeout(
            ProcessCommand::new(
                "cmd.exe",
                [
                    "/D".to_string(),
                    "/C".to_string(),
                    script.to_string_lossy().into_owned(),
                ],
            ),
            Some(Duration::from_millis(100)),
        )
        .expect_err("the delayed child must time out");
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(script);

        assert!(error.to_string().contains("命令执行超时"));
    }

    #[test]
    fn output_limit_stops_a_chatty_process_with_bounded_observation() {
        let path =
            std::env::temp_dir().join(format!("nwflash-output-limit-{}.txt", std::process::id()));
        let script = path.with_extension("cmd");
        std::fs::write(
            &script,
            format!(
                "@echo off\r\ntype \"{}\"\r\nping 127.0.0.1 -n 10 > nul\r\n",
                path.to_string_lossy()
            ),
        )
        .expect("output-limit script should be written");
        std::fs::write(&path, vec![b'x'; PROCESS_OUTPUT_MAX_BYTES + 1])
            .expect("oversized fixture should be written");
        let outcome = run_command_with_cancel_observed(
            ProcessCommand::new(
                "cmd.exe",
                [
                    "/D".to_string(),
                    "/C".to_string(),
                    script.to_string_lossy().into_owned(),
                ],
            ),
            Some(Duration::from_secs(10)),
            || false,
            Arc::new(RecordingObserver::default()),
        );
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(script);
        let error = match outcome.result {
            Err(error) => error,
            Ok(output) => panic!("chatty process unexpectedly completed: {:?}", output.stderr),
        };
        assert!(error.to_string().contains("输出超过安全上限"));
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
