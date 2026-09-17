use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use nwflash_application::OperationCoordinatorError;
use nwflash_domain::{DeviceConnectionState, DeviceRefreshMode, DeviceSnapshot, DomainError};
use nwflash_windows::process::{CancellableProcessExecutor, ProcessCommand, ProcessOutput};
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use crate::AppState;

const E2E_SERIAL: &str = "E2E-FILE-DEVICE";
const PENDING_LIMIT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scenario {
    Unconfigured,
    UploadSuccess,
    UploadNonzero,
    UploadSpawn,
    UploadTimeout,
    UploadOutput,
    UploadCancel,
    UploadFailOnce,
    UploadConflict,
    UploadCleanupFailure,
    DownloadSuccess,
    DownloadFailure,
    DownloadCancel,
    DownloadExisting,
    DownloadRace,
    InstallSuccess,
    InstallFailure,
    InstallCancel,
}

impl Scenario {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "upload-success" => Ok(Self::UploadSuccess),
            "upload-nonzero" => Ok(Self::UploadNonzero),
            "upload-spawn" => Ok(Self::UploadSpawn),
            "upload-timeout" => Ok(Self::UploadTimeout),
            "upload-output" => Ok(Self::UploadOutput),
            "upload-cancel" => Ok(Self::UploadCancel),
            "upload-fail-once" => Ok(Self::UploadFailOnce),
            "upload-conflict" => Ok(Self::UploadConflict),
            "upload-cleanup-failure" => Ok(Self::UploadCleanupFailure),
            "download-success" => Ok(Self::DownloadSuccess),
            "download-failure" => Ok(Self::DownloadFailure),
            "download-cancel" => Ok(Self::DownloadCancel),
            "download-existing" => Ok(Self::DownloadExisting),
            "download-race" => Ok(Self::DownloadRace),
            "install-success" => Ok(Self::InstallSuccess),
            "install-failure" => Ok(Self::InstallFailure),
            "install-cancel" => Ok(Self::InstallCancel),
            _ => Err("unknown file E2E scenario".to_string()),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Unconfigured => "unconfigured",
            Self::UploadSuccess => "upload-success",
            Self::UploadNonzero => "upload-nonzero",
            Self::UploadSpawn => "upload-spawn",
            Self::UploadTimeout => "upload-timeout",
            Self::UploadOutput => "upload-output",
            Self::UploadCancel => "upload-cancel",
            Self::UploadFailOnce => "upload-fail-once",
            Self::UploadConflict => "upload-conflict",
            Self::UploadCleanupFailure => "upload-cleanup-failure",
            Self::DownloadSuccess => "download-success",
            Self::DownloadFailure => "download-failure",
            Self::DownloadCancel => "download-cancel",
            Self::DownloadExisting => "download-existing",
            Self::DownloadRace => "download-race",
            Self::InstallSuccess => "install-success",
            Self::InstallFailure => "install-failure",
            Self::InstallCancel => "install-cancel",
        }
    }
}

#[derive(Debug)]
struct Sandbox {
    root: PathBuf,
    source: PathBuf,
    destination: PathBuf,
    apk: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileE2eFixtureDto {
    source_path: String,
    destination_path: String,
    apk_path: String,
    remote_path: String,
    device_snapshot: DeviceSnapshot,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct LedgerEntry {
    sequence: u64,
    stage: String,
    outcome: String,
    timeout_ms: Option<u64>,
    serial_from_rust: bool,
    transaction_temp: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileE2eSnapshotDto {
    scenario: String,
    entries: Vec<LedgerEntry>,
    operation_busy: bool,
    remote_temp_exists: bool,
    remote_final_exists: bool,
    destination_state: String,
    local_partial_count: usize,
    source_preserved: bool,
    apk_preserved: bool,
}

#[derive(Debug)]
struct HarnessState {
    scenario: Scenario,
    sandbox: Option<Sandbox>,
    entries: Vec<LedgerEntry>,
    next_sequence: u64,
    transfer_attempts: usize,
    remote_temp_exists: bool,
    remote_final_exists: bool,
}

impl Default for HarnessState {
    fn default() -> Self {
        Self {
            scenario: Scenario::Unconfigured,
            sandbox: None,
            entries: Vec::new(),
            next_sequence: 1,
            transfer_attempts: 0,
            remote_temp_exists: false,
            remote_final_exists: false,
        }
    }
}

#[derive(Default)]
struct FileE2eHarness {
    state: Mutex<HarnessState>,
}

impl FileE2eHarness {
    fn configure(&self, scenario: Scenario) -> Result<FileE2eFixtureDto, String> {
        let previous = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| "file E2E harness lock failed")?;
            state.sandbox.take()
        };
        if let Some(previous) = previous {
            cleanup_sandbox(&previous.root)?;
        }

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "file E2E clock failed")?
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "nwflash-file-e2e-{}-{}-{nonce}",
            std::process::id(),
            scenario.label()
        ));
        fs::create_dir(&root).map_err(|_| "file E2E sandbox creation failed")?;
        let source = root.join("upload-source.bin");
        let destination = root.join("download-destination.bin");
        let apk = root.join("manager.apk");
        fs::write(&source, b"e2e-upload-source")
            .map_err(|_| "file E2E upload fixture creation failed")?;
        fs::write(&apk, b"e2e-apk-source").map_err(|_| "file E2E APK fixture creation failed")?;
        if scenario == Scenario::DownloadExisting {
            fs::write(&destination, b"old-destination")
                .map_err(|_| "file E2E existing destination creation failed")?;
        }

        let fixture = FileE2eFixtureDto {
            source_path: source.to_string_lossy().into_owned(),
            destination_path: destination.to_string_lossy().into_owned(),
            apk_path: apk.to_string_lossy().into_owned(),
            remote_path: "/sdcard/Download/payload.bin".to_string(),
            device_snapshot: fixture_device_snapshot(),
        };
        let mut state = self
            .state
            .lock()
            .map_err(|_| "file E2E harness lock failed")?;
        *state = HarnessState {
            scenario,
            sandbox: Some(Sandbox {
                root,
                source,
                destination,
                apk,
            }),
            remote_final_exists: scenario == Scenario::UploadConflict,
            ..HarnessState::default()
        };
        Ok(fixture)
    }

    fn reset(&self) -> Result<(), String> {
        let sandbox = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| "file E2E harness lock failed")?;
            state.sandbox.take()
        };
        if let Some(sandbox) = sandbox {
            cleanup_sandbox(&sandbox.root)?;
        }
        *self
            .state
            .lock()
            .map_err(|_| "file E2E harness lock failed")? = HarnessState::default();
        Ok(())
    }

    fn record(
        &self,
        command: &ProcessCommand,
        stage: &str,
        outcome: &str,
        timeout: Option<Duration>,
    ) {
        if let Ok(mut state) = self.state.lock() {
            let sequence = state.next_sequence;
            state.next_sequence = state.next_sequence.saturating_add(1);
            state.entries.push(LedgerEntry {
                sequence,
                stage: stage.to_string(),
                outcome: outcome.to_string(),
                timeout_ms: timeout.map(|value| value.as_millis().min(u128::from(u64::MAX)) as u64),
                serial_from_rust: command.args.first().is_some_and(|value| value == "-s")
                    && command.args.get(1).is_some_and(|value| value == E2E_SERIAL),
                transaction_temp: command.args.iter().any(|argument| {
                    argument.contains(".nwflash-upload-") || argument.contains(".nwflash-download-")
                }),
            });
        }
    }

    fn record_terminal(&self, outcome: &str) {
        if let Ok(mut state) = self.state.lock() {
            let sequence = state.next_sequence;
            state.next_sequence = state.next_sequence.saturating_add(1);
            state.entries.push(LedgerEntry {
                sequence,
                stage: "transaction".to_string(),
                outcome: outcome.to_string(),
                timeout_ms: None,
                serial_from_rust: true,
                transaction_temp: false,
            });
        }
    }

    fn snapshot(&self, operation_busy: bool) -> FileE2eSnapshotDto {
        let state = self
            .state
            .lock()
            .expect("file E2E harness lock should not be poisoned");
        let (destination_state, local_partial_count, source_preserved, apk_preserved) = state
            .sandbox
            .as_ref()
            .map(|sandbox| {
                let destination_state = match fs::read(&sandbox.destination) {
                    Ok(bytes) if bytes == b"downloaded-by-e2e" => "downloaded",
                    Ok(bytes) if bytes == b"old-destination" => "old",
                    Ok(_) => "other",
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => "absent",
                    Err(_) => "unreadable",
                };
                let local_partial_count = fs::read_dir(&sandbox.root)
                    .map(|entries| {
                        entries
                            .flatten()
                            .filter(|entry| {
                                entry
                                    .file_name()
                                    .to_string_lossy()
                                    .contains(".nwflash-download-")
                            })
                            .count()
                    })
                    .unwrap_or(usize::MAX);
                (
                    destination_state.to_string(),
                    local_partial_count,
                    fs::read(&sandbox.source).is_ok_and(|bytes| bytes == b"e2e-upload-source"),
                    fs::read(&sandbox.apk).is_ok_and(|bytes| bytes == b"e2e-apk-source"),
                )
            })
            .unwrap_or_else(|| ("absent".to_string(), 0, false, false));
        FileE2eSnapshotDto {
            scenario: state.scenario.label().to_string(),
            entries: state.entries.clone(),
            operation_busy,
            remote_temp_exists: state.remote_temp_exists,
            remote_final_exists: state.remote_final_exists,
            destination_state,
            local_partial_count,
            source_preserved,
            apk_preserved,
        }
    }

    fn scenario(&self) -> Scenario {
        self.state
            .lock()
            .map(|state| state.scenario)
            .unwrap_or(Scenario::Unconfigured)
    }

    fn note_transfer_start(&self, remote: bool) -> usize {
        let mut state = self
            .state
            .lock()
            .expect("file E2E harness lock should not be poisoned");
        state.transfer_attempts = state.transfer_attempts.saturating_add(1);
        if remote {
            state.remote_temp_exists = true;
        }
        state.transfer_attempts
    }

    fn complete_remote_promote(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.remote_temp_exists = false;
            state.remote_final_exists = true;
        }
    }

    fn complete_remote_cleanup(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.remote_temp_exists = false;
        }
    }

    fn write_pull_fixture(&self, command: &ProcessCommand, race: bool) -> Result<(), DomainError> {
        let temporary = command.args.last().ok_or_else(|| {
            DomainError::Internal("file E2E pull command lacks a destination".to_string())
        })?;
        fs::write(temporary, b"downloaded-by-e2e")
            .map_err(|_| DomainError::Internal("file E2E pull fixture failed".to_string()))?;
        if race {
            let destination = self
                .state
                .lock()
                .ok()
                .and_then(|state| {
                    state
                        .sandbox
                        .as_ref()
                        .map(|sandbox| sandbox.destination.clone())
                })
                .ok_or_else(|| DomainError::Internal("file E2E sandbox missing".to_string()))?;
            fs::write(destination, b"old-destination")
                .map_err(|_| DomainError::Internal("file E2E race fixture failed".to_string()))?;
        }
        Ok(())
    }

    fn wait_for_cancel(
        &self,
        should_cancel: &mut dyn FnMut() -> bool,
    ) -> Result<ProcessOutput, DomainError> {
        let started = Instant::now();
        while !should_cancel() {
            if started.elapsed() >= PENDING_LIMIT {
                return Err(DomainError::Internal(
                    "file E2E pending fixture exceeded its deadline".to_string(),
                ));
            }
            thread::sleep(Duration::from_millis(10));
        }
        Err(DomainError::UserCancelled(
            "file E2E operation canceled".to_string(),
        ))
    }
}

impl CancellableProcessExecutor for FileE2eHarness {
    fn run(
        &self,
        command: ProcessCommand,
        should_cancel: &mut dyn FnMut() -> bool,
    ) -> Result<ProcessOutput, DomainError> {
        self.run_with_timeout(command, None, should_cancel)
    }

    fn run_with_timeout(
        &self,
        command: ProcessCommand,
        timeout: Option<Duration>,
        should_cancel: &mut dyn FnMut() -> bool,
    ) -> Result<ProcessOutput, DomainError> {
        let stage = command_stage(&command);
        self.record(&command, stage, "started", timeout);
        let scenario = self.scenario();
        let result = match stage {
            "push" => {
                let attempt = self.note_transfer_start(true);
                match scenario {
                    Scenario::UploadSuccess | Scenario::UploadConflict => ok(),
                    Scenario::UploadNonzero => nonzero(7),
                    Scenario::UploadSpawn => Err(DomainError::ExternalTool(
                        "file E2E spawn failure".to_string(),
                    )),
                    Scenario::UploadTimeout => Err(DomainError::ExternalTool(
                        "命令执行超时，fixture 已终止。".to_string(),
                    )),
                    Scenario::UploadOutput => Err(DomainError::ExternalTool(
                        "命令输出超过安全上限。".to_string(),
                    )),
                    Scenario::UploadCancel => self.wait_for_cancel(should_cancel),
                    Scenario::UploadFailOnce if attempt == 1 => nonzero(7),
                    Scenario::UploadFailOnce => ok(),
                    Scenario::UploadCleanupFailure => nonzero(7),
                    _ => Err(unexpected_stage(scenario, stage)),
                }
            }
            "promote" => match scenario {
                Scenario::UploadSuccess | Scenario::UploadFailOnce => {
                    self.complete_remote_promote();
                    ok()
                }
                Scenario::UploadConflict => nonzero(73),
                _ => Err(unexpected_stage(scenario, stage)),
            },
            "cleanup" => {
                if scenario == Scenario::UploadCleanupFailure {
                    Err(DomainError::ExternalTool(
                        "file E2E cleanup failure".to_string(),
                    ))
                } else {
                    self.complete_remote_cleanup();
                    ok()
                }
            }
            "pull" => {
                self.note_transfer_start(false);
                match scenario {
                    Scenario::DownloadSuccess => {
                        self.write_pull_fixture(&command, false)?;
                        ok()
                    }
                    Scenario::DownloadRace => {
                        self.write_pull_fixture(&command, true)?;
                        ok()
                    }
                    Scenario::DownloadFailure => {
                        self.write_pull_fixture(&command, false)?;
                        nonzero(9)
                    }
                    Scenario::DownloadCancel => {
                        self.write_pull_fixture(&command, false)?;
                        self.wait_for_cancel(should_cancel)
                    }
                    _ => Err(unexpected_stage(scenario, stage)),
                }
            }
            "install" => match scenario {
                Scenario::InstallSuccess => ok(),
                Scenario::InstallFailure => nonzero(1),
                Scenario::InstallCancel => self.wait_for_cancel(should_cancel),
                _ => Err(unexpected_stage(scenario, stage)),
            },
            _ => Err(unexpected_stage(scenario, stage)),
        };
        let outcome = match &result {
            Ok(output) => format!("exit-{}", output.exit_code),
            Err(DomainError::UserCancelled(_)) => "canceled".to_string(),
            Err(_) => "error".to_string(),
        };
        self.record(&command, stage, &outcome, timeout);
        result
    }
}

fn command_stage(command: &ProcessCommand) -> &'static str {
    match command.args.get(2).map(String::as_str) {
        Some("push") => "push",
        Some("pull") => "pull",
        Some("install") => "install",
        Some("shell")
            if command
                .args
                .last()
                .is_some_and(|value| value.contains("mv -n --")) =>
        {
            "promote"
        }
        Some("shell")
            if command
                .args
                .last()
                .is_some_and(|value| value.contains("rm -f --")) =>
        {
            "cleanup"
        }
        _ => "unexpected",
    }
}

fn ok() -> Result<ProcessOutput, DomainError> {
    Ok(ProcessOutput {
        exit_code: 0,
        stdout: String::new(),
        stderr: String::new(),
    })
}

fn nonzero(exit_code: i32) -> Result<ProcessOutput, DomainError> {
    Ok(ProcessOutput {
        exit_code,
        stdout: String::new(),
        stderr: "e2e fixture failure".to_string(),
    })
}

fn unexpected_stage(scenario: Scenario, stage: &str) -> DomainError {
    DomainError::Internal(format!(
        "file E2E scenario {} rejected stage {stage}",
        scenario.label()
    ))
}

fn harness() -> Arc<FileE2eHarness> {
    static HARNESS: OnceLock<Arc<FileE2eHarness>> = OnceLock::new();
    HARNESS
        .get_or_init(|| Arc::new(FileE2eHarness::default()))
        .clone()
}

pub(super) fn executor() -> Arc<dyn CancellableProcessExecutor> {
    harness()
}

pub(super) fn record_terminal(result: &Result<(), OperationCoordinatorError>) {
    let outcome = match result {
        Ok(()) => "success",
        Err(OperationCoordinatorError::Canceled) => "canceled",
        Err(OperationCoordinatorError::InProgress) => "in-progress",
        Err(_) => "failed",
    };
    harness().record_terminal(outcome);
}

#[tauri::command]
pub async fn file_e2e_configure(
    state: State<'_, AppState>,
    app_handle: AppHandle,
    scenario: String,
) -> Result<FileE2eFixtureDto, String> {
    let _idle = state
        .operation_coordinator
        .try_acquire_idle()
        .map_err(|_| "file E2E configure requires an idle coordinator".to_string())?;
    let fixture = harness().configure(Scenario::parse(&scenario)?)?;
    state.device_runtime.apply_snapshot(
        fixture.device_snapshot.clone(),
        false,
        DeviceRefreshMode::Manual,
    );
    app_handle
        .emit("device:snapshot", state.device_runtime.overview_payload())
        .map_err(|_| "file E2E device event failed".to_string())?;
    Ok(fixture)
}

fn fixture_device_snapshot() -> DeviceSnapshot {
    DeviceSnapshot {
        connection_state: DeviceConnectionState::AdbConnected,
        serial: E2E_SERIAL.to_string(),
        connection_label: "ADB E2E 已连接".to_string(),
        model: "FILE-E2E".to_string(),
        android_version: "fixture".to_string(),
        battery_level: "100%".to_string(),
    }
}

#[tauri::command]
pub async fn file_e2e_snapshot(state: State<'_, AppState>) -> Result<FileE2eSnapshotDto, String> {
    Ok(harness().snapshot(state.operation_coordinator.is_busy()))
}

#[tauri::command]
pub async fn file_e2e_reset(state: State<'_, AppState>) -> Result<(), String> {
    let _idle = state
        .operation_coordinator
        .try_acquire_idle()
        .map_err(|_| "file E2E reset requires an idle coordinator".to_string())?;
    harness().reset()
}

fn cleanup_sandbox(root: &Path) -> Result<(), String> {
    if !root.exists() {
        return Ok(());
    }
    let base = std::env::temp_dir()
        .canonicalize()
        .map_err(|_| "file E2E temp root validation failed")?;
    let resolved = root
        .canonicalize()
        .map_err(|_| "file E2E sandbox validation failed")?;
    let owned_name = resolved
        .file_name()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.starts_with("nwflash-file-e2e-"));
    if !owned_name || resolved == base || !resolved.starts_with(&base) {
        return Err("file E2E sandbox ownership validation failed".to_string());
    }
    fs::remove_dir_all(&resolved).map_err(|_| "file E2E sandbox cleanup failed".to_string())
}
