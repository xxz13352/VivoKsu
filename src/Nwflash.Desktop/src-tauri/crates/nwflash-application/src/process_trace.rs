use std::{collections::VecDeque, fmt, io::Cursor, sync::Mutex};

use nwflash_domain::{TraceEventKindV2, TraceEventStatusV2, TraceId, TraceOutputStreamV2};
use nwflash_protection::{
    ExactSecretSet, SentinelAttestedTraceUpload, TraceCommandText, TraceEventText,
    TraceOutputSession, TraceRedactionError,
};
use nwflash_windows::process::{
    ObservedProcessOutcome, ProcessFinishMetadata, ProcessObservation, ProcessObserverError,
    ProcessObserverLoss, ProcessOutputObserver, ProcessOutputStream as ObservedProcessOutputStream,
    ProcessStartMetadata, ProcessStdoutMode, ProcessTermination,
};

use crate::{
    TraceIdentityProvider, TraceMetadataSink, TraceProducerError, TraceRunHandle,
    TraceTerminalOutcome,
};

/// Raw process output retained until the process observer has drained. The
/// allocation is fixed up front, so growing the logical stream cannot leave
/// allocator-owned copies of an earlier raw buffer behind.
const PROCESS_TRACE_OUTPUT_LIMIT_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessTraceLoss {
    Observer,
    OutputLimit,
    TerminationUnconfirmed,
    IncompleteOutput,
    HighRisk,
    Redaction,
}

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum ProcessTraceError {
    #[error("process trace ID generation failed")]
    IdGeneration,
    #[error("process trace collector allocation failed")]
    Allocation,
    #[error("process trace collector has already been consumed")]
    AlreadyConsumed,
    #[error("process trace observation was incomplete")]
    IncompleteObservation,
    #[error("process trace was terminated after a loss")]
    Loss(ProcessTraceLoss),
    #[error("trace producer rejected the sealed process event")]
    Producer(TraceProducerError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessTraceCommit {
    event_ids: Vec<TraceId>,
    upload_ids: Vec<TraceId>,
}

impl ProcessTraceCommit {
    pub fn event_ids(&self) -> &[TraceId] {
        &self.event_ids
    }

    pub fn upload_ids(&self) -> &[TraceId] {
        &self.upload_ids
    }

    pub fn attempt_count(&self) -> usize {
        self.upload_ids.len()
    }
}

struct RawOutput {
    bytes: Vec<u8>,
}

impl RawOutput {
    fn try_with_capacity(capacity: usize) -> Result<Self, ProcessTraceError> {
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(capacity)
            .map_err(|_| ProcessTraceError::Allocation)?;
        Ok(Self { bytes })
    }

    fn extend_from_slice(&mut self, bytes: &[u8]) {
        debug_assert!(self.bytes.len() + bytes.len() <= self.bytes.capacity());
        self.bytes.extend_from_slice(bytes);
    }

    fn len(&self) -> usize {
        self.bytes.len()
    }

    fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    fn as_slice(&self) -> &[u8] {
        &self.bytes
    }
}

impl Drop for RawOutput {
    fn drop(&mut self) {
        self.bytes.fill(0);
    }
}

struct ProcessTraceState {
    started: Option<ProcessStartMetadata>,
    finished: Option<ProcessFinishMetadata>,
    stdout: RawOutput,
    stderr: RawOutput,
    next_stdout_sequence: u64,
    next_stderr_sequence: u64,
    loss: Option<ProcessTraceLoss>,
    outcome_consumed: bool,
    pending: Option<PendingProcessTrace>,
}

struct PendingProcessEvent {
    sequence: u64,
    uploads: Vec<SentinelAttestedTraceUpload>,
    reserved: bool,
}

struct PendingProcessTrace {
    events: VecDeque<PendingProcessEvent>,
    event_ids: Vec<TraceId>,
    upload_ids: Vec<TraceId>,
}

/// Synchronous, bounded receiver for [`ProcessOutputObserver`] callbacks.
///
/// This type never performs redaction, serialization, disk I/O or HTTP from a
/// callback. Call [`Self::finish_observed_into_run`] only after the observed
/// process outcome has drained its callback queue. The complete stdout/stderr
/// streams then cross the protection boundary exactly once, outside the
/// callback.
pub struct ProcessTraceCollector {
    step_name: String,
    started_at_ms: u64,
    stdout_event_id: TraceId,
    stderr_event_id: TraceId,
    exact_secrets: ExactSecretSet,
    output_limit: usize,
    state: Mutex<ProcessTraceState>,
}

impl ProcessTraceCollector {
    pub fn try_new(
        step_name: impl Into<String>,
        started_at_ms: u64,
        exact_secrets: ExactSecretSet,
    ) -> Result<Self, ProcessTraceError> {
        Self::try_new_with_limit(
            step_name.into(),
            started_at_ms,
            exact_secrets,
            PROCESS_TRACE_OUTPUT_LIMIT_BYTES,
        )
    }

    #[cfg(test)]
    fn try_new_with_output_limit(
        step_name: impl Into<String>,
        started_at_ms: u64,
        exact_secrets: ExactSecretSet,
        output_limit: usize,
    ) -> Result<Self, ProcessTraceError> {
        Self::try_new_with_limit(step_name.into(), started_at_ms, exact_secrets, output_limit)
    }

    fn try_new_with_limit(
        step_name: String,
        started_at_ms: u64,
        exact_secrets: ExactSecretSet,
        output_limit: usize,
    ) -> Result<Self, ProcessTraceError> {
        let stdout_event_id = TraceId::try_new_v7().map_err(|_| ProcessTraceError::IdGeneration)?;
        let stderr_event_id = TraceId::try_new_v7().map_err(|_| ProcessTraceError::IdGeneration)?;
        Ok(Self {
            step_name,
            started_at_ms,
            stdout_event_id,
            stderr_event_id,
            exact_secrets,
            output_limit,
            state: Mutex::new(ProcessTraceState {
                started: None,
                finished: None,
                stdout: RawOutput::try_with_capacity(output_limit)?,
                stderr: RawOutput::try_with_capacity(output_limit)?,
                next_stdout_sequence: 0,
                next_stderr_sequence: 0,
                loss: None,
                outcome_consumed: false,
                pending: None,
            }),
        })
    }

    /// Completes collection only after the process layer has drained its
    /// bounded callback queue. Requiring the full observed outcome makes it
    /// impossible for a normal caller to forget the observer-loss sideband.
    /// The legacy result's stdout/stderr strings are intentionally never read.
    pub fn finish_observed_into_run<I, S>(
        &self,
        run: &TraceRunHandle<I, S>,
        outcome: ObservedProcessOutcome,
        ended_at_ms: u64,
    ) -> Result<ProcessTraceCommit, ProcessTraceError>
    where
        I: TraceIdentityProvider + 'static,
        S: TraceMetadataSink + 'static,
    {
        self.finish_into_run(run, &outcome.observer_losses, ended_at_ms)
    }

    /// Retries only the exact sentinel-attested attempts retained after a sink
    /// failure. No raw output is rescanned and no upload or event ID is minted.
    pub fn retry_pending_into_run<I, S>(
        &self,
        run: &TraceRunHandle<I, S>,
    ) -> Result<ProcessTraceCommit, ProcessTraceError>
    where
        I: TraceIdentityProvider + 'static,
        S: TraceMetadataSink + 'static,
    {
        self.flush_pending(run)
    }

    fn finish_into_run<I, S>(
        &self,
        run: &TraceRunHandle<I, S>,
        observer_losses: &[ProcessObserverLoss],
        ended_at_ms: u64,
    ) -> Result<ProcessTraceCommit, ProcessTraceError>
    where
        I: TraceIdentityProvider + 'static,
        S: TraceMetadataSink + 'static,
    {
        let (started, finished, stdout, stderr, internal_loss) = {
            let mut state = self.state.lock().unwrap_or_else(|value| value.into_inner());
            if state.outcome_consumed {
                return Err(ProcessTraceError::AlreadyConsumed);
            }
            state.outcome_consumed = true;
            (
                state.started.take(),
                state.finished.take(),
                std::mem::replace(&mut state.stdout, RawOutput::try_with_capacity(0)?),
                std::mem::replace(&mut state.stderr, RawOutput::try_with_capacity(0)?),
                state.loss,
            )
        };

        if !observer_losses.is_empty() {
            return fail_run(run, ProcessTraceLoss::Observer);
        }
        if let Some(loss) = internal_loss {
            return fail_run(run, loss);
        }
        let (Some(started), Some(finished)) = (started, finished) else {
            run.finalize(TraceTerminalOutcome::Failed)
                .map_err(ProcessTraceError::Producer)?;
            return Err(ProcessTraceError::IncompleteObservation);
        };
        match finished.termination {
            ProcessTermination::TerminationUnconfirmed => {
                return fail_run(run, ProcessTraceLoss::TerminationUnconfirmed)
            }
            ProcessTermination::OutputReadFailed => {
                return fail_run(run, ProcessTraceLoss::IncompleteOutput)
            }
            _ => {}
        }

        let status = event_status(finished);
        let error_code = event_error_code(finished);
        let mut streams = Vec::new();
        let mut stdout = Some(stdout);
        let mut stderr = Some(stderr);
        if started.stdout_mode == ProcessStdoutMode::TextPipe
            && stdout.as_ref().is_some_and(|output| !output.is_empty())
        {
            streams.push((
                ObservedProcessOutputStream::Stdout,
                self.stdout_event_id,
                stdout.take().expect("stdout is present"),
            ));
        }
        if stderr.as_ref().is_some_and(|output| !output.is_empty()) {
            streams.push((
                ObservedProcessOutputStream::Stderr,
                self.stderr_event_id,
                stderr.take().expect("stderr is present"),
            ));
        }
        if streams.is_empty() {
            if started.stdout_mode == ProcessStdoutMode::TextPipe {
                streams.push((
                    ObservedProcessOutputStream::Stdout,
                    self.stdout_event_id,
                    stdout.take().expect("stdout is present"),
                ));
            } else {
                streams.push((
                    ObservedProcessOutputStream::Stderr,
                    self.stderr_event_id,
                    stderr.take().expect("stderr is present"),
                ));
            }
        }

        let argv = started.args.iter().map(String::as_str).collect::<Vec<_>>();
        let working_directory = started
            .working_directory
            .as_ref()
            .and_then(|path| path.to_str());
        let paths = working_directory.into_iter().collect::<Vec<_>>();
        let command = TraceCommandText {
            program: &started.program,
            argv: &argv,
            display_command: &started.program,
            working_directory,
            paths: &paths,
            urls: &[],
            serial: None,
        };
        let ended_at_ms = ended_at_ms.max(self.started_at_ms);
        let duration_ms = ended_at_ms.saturating_sub(self.started_at_ms);
        let mut planned = VecDeque::new();
        let mut event_ids = Vec::new();
        let mut upload_ids = Vec::new();

        for (stream, event_id, output) in streams {
            let sequence = run
                .next_sequence()
                .checked_add(planned.len() as u64)
                .ok_or(ProcessTraceError::Producer(
                    TraceProducerError::SequenceLimit,
                ))?;
            let mut reader = Cursor::new(output.as_slice());
            let session = TraceOutputSession::from_reader(
                event_id,
                domain_stream(stream),
                &mut reader,
                &self.exact_secrets,
            )
            .map_err(|error| redaction_error(run, error))?;
            let sealed_attempts = session
                .into_event_upload_attempts(
                    TraceEventText {
                        event_id,
                        run_id: run.run_id(),
                        sequence,
                        kind: TraceEventKindV2::Command,
                        step_name: &self.step_name,
                        partition_name: None,
                        status,
                        started_at_ms: self.started_at_ms,
                        ended_at_ms: Some(ended_at_ms),
                        duration_ms: Some(duration_ms),
                        command: Some(command),
                        exit_code: finished.exit_code,
                        verification: None,
                        device_state: None,
                        retry_safe: None,
                        remedies: &[],
                        error_class: error_code.map(|_| "process"),
                        error_code,
                        error_message: None,
                    },
                    &self.exact_secrets,
                )
                .map_err(|error| redaction_error(run, error))?;
            let attempts = sealed_attempts
                .into_iter()
                .map(SentinelAttestedTraceUpload::try_from)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| redaction_error(run, error))?;
            event_ids.push(event_id);
            upload_ids.extend(attempts.iter().map(SentinelAttestedTraceUpload::upload_id));
            planned.push_back(PendingProcessEvent {
                sequence,
                uploads: attempts,
                reserved: false,
            });
        }

        {
            let mut state = self.state.lock().unwrap_or_else(|value| value.into_inner());
            state.pending = Some(PendingProcessTrace {
                events: planned,
                event_ids,
                upload_ids,
            });
        }
        self.flush_pending(run)
    }

    fn flush_pending<I, S>(
        &self,
        run: &TraceRunHandle<I, S>,
    ) -> Result<ProcessTraceCommit, ProcessTraceError>
    where
        I: TraceIdentityProvider + 'static,
        S: TraceMetadataSink + 'static,
    {
        loop {
            let mut state = self.state.lock().unwrap_or_else(|value| value.into_inner());
            let pending = state
                .pending
                .as_mut()
                .ok_or(ProcessTraceError::AlreadyConsumed)?;
            let Some(event) = pending.events.front_mut() else {
                let completed = state
                    .pending
                    .take()
                    .expect("pending trace exists until completion");
                return Ok(ProcessTraceCommit {
                    event_ids: completed.event_ids,
                    upload_ids: completed.upload_ids,
                });
            };
            if !event.reserved {
                let reserved = run
                    .reserve_sequence(event.sequence)
                    .map_err(ProcessTraceError::Producer)?;
                debug_assert_eq!(reserved, event.sequence);
                event.reserved = true;
            }
            run.append_upload_attempts(event.sequence, &event.uploads)
                .map_err(ProcessTraceError::Producer)?;
            pending
                .events
                .pop_front()
                .expect("front event was just appended");
        }
    }
}

impl ProcessOutputObserver for ProcessTraceCollector {
    fn observe(&self, observation: ProcessObservation<'_>) -> Result<(), ProcessObserverError> {
        let mut state = self.state.lock().unwrap_or_else(|value| value.into_inner());
        if state.outcome_consumed {
            return Err(ProcessObserverError);
        }
        match observation {
            ProcessObservation::Started(metadata) => {
                if state.started.is_some() {
                    state.loss = Some(ProcessTraceLoss::Observer);
                    return Err(ProcessObserverError);
                }
                state.started = Some(metadata.clone());
            }
            ProcessObservation::Output {
                stream,
                sequence,
                bytes,
            } => {
                let Some(started) = state.started.as_ref() else {
                    state.loss = Some(ProcessTraceLoss::Observer);
                    return Err(ProcessObserverError);
                };
                if stream == ObservedProcessOutputStream::Stdout {
                    match started.stdout_mode {
                        ProcessStdoutMode::BinaryFile => return Ok(()),
                        ProcessStdoutMode::Unavailable => {
                            state.loss = Some(ProcessTraceLoss::Observer);
                            return Err(ProcessObserverError);
                        }
                        ProcessStdoutMode::TextPipe => {}
                    }
                }
                let total = state
                    .stdout
                    .len()
                    .checked_add(state.stderr.len())
                    .and_then(|value| value.checked_add(bytes.len()));
                if total.is_none_or(|value| value > self.output_limit) {
                    state.loss = Some(ProcessTraceLoss::OutputLimit);
                    return Err(ProcessObserverError);
                }
                match stream {
                    ObservedProcessOutputStream::Stdout => {
                        if sequence != state.next_stdout_sequence {
                            state.loss = Some(ProcessTraceLoss::Observer);
                            return Err(ProcessObserverError);
                        }
                        state.stdout.extend_from_slice(bytes);
                        state.next_stdout_sequence = state.next_stdout_sequence.saturating_add(1);
                    }
                    ObservedProcessOutputStream::Stderr => {
                        if sequence != state.next_stderr_sequence {
                            state.loss = Some(ProcessTraceLoss::Observer);
                            return Err(ProcessObserverError);
                        }
                        state.stderr.extend_from_slice(bytes);
                        state.next_stderr_sequence = state.next_stderr_sequence.saturating_add(1);
                    }
                }
            }
            ProcessObservation::Finished(metadata) => {
                if state.finished.replace(metadata).is_some() {
                    state.loss = Some(ProcessTraceLoss::Observer);
                    return Err(ProcessObserverError);
                }
            }
        }
        Ok(())
    }
}

impl fmt::Debug for ProcessTraceCollector {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.state.lock().unwrap_or_else(|value| value.into_inner());
        formatter
            .debug_struct("ProcessTraceCollector")
            .field("stdout_event_id", &self.stdout_event_id)
            .field("stderr_event_id", &self.stderr_event_id)
            .field("has_started", &state.started.is_some())
            .field("has_finished", &state.finished.is_some())
            .field("stdout_bytes", &state.stdout.len())
            .field("stderr_bytes", &state.stderr.len())
            .field("loss", &state.loss)
            .field("outcome_consumed", &state.outcome_consumed)
            .field(
                "pending_event_count",
                &state
                    .pending
                    .as_ref()
                    .map_or(0, |pending| pending.events.len()),
            )
            .finish()
    }
}

fn domain_stream(stream: ObservedProcessOutputStream) -> TraceOutputStreamV2 {
    match stream {
        ObservedProcessOutputStream::Stdout => TraceOutputStreamV2::Stdout,
        ObservedProcessOutputStream::Stderr => TraceOutputStreamV2::Stderr,
    }
}

fn event_status(finished: ProcessFinishMetadata) -> TraceEventStatusV2 {
    match finished.termination {
        ProcessTermination::Completed if finished.exit_code == Some(0) => {
            TraceEventStatusV2::Success
        }
        ProcessTermination::Cancelled | ProcessTermination::TimedOut => {
            TraceEventStatusV2::Canceled
        }
        ProcessTermination::Completed
        | ProcessTermination::SpawnFailed
        | ProcessTermination::WaitFailed
        | ProcessTermination::OutputReadFailed
        | ProcessTermination::TerminationUnconfirmed => TraceEventStatusV2::Failed,
    }
}

fn event_error_code(finished: ProcessFinishMetadata) -> Option<&'static str> {
    match finished.termination {
        ProcessTermination::Completed if finished.exit_code == Some(0) => None,
        ProcessTermination::Completed => Some("nonzero_exit"),
        ProcessTermination::SpawnFailed => Some("spawn_failed"),
        ProcessTermination::WaitFailed => Some("wait_failed"),
        ProcessTermination::OutputReadFailed => Some("output_incomplete"),
        ProcessTermination::Cancelled => Some("canceled"),
        ProcessTermination::TimedOut => Some("timed_out"),
        ProcessTermination::TerminationUnconfirmed => Some("termination_unconfirmed"),
    }
}

fn fail_run<I, S>(
    run: &TraceRunHandle<I, S>,
    loss: ProcessTraceLoss,
) -> Result<ProcessTraceCommit, ProcessTraceError>
where
    I: TraceIdentityProvider + 'static,
    S: TraceMetadataSink + 'static,
{
    run.finalize(TraceTerminalOutcome::Failed)
        .map_err(ProcessTraceError::Producer)?;
    Err(ProcessTraceError::Loss(loss))
}

fn redaction_error<I, S>(
    run: &TraceRunHandle<I, S>,
    error: TraceRedactionError,
) -> ProcessTraceError
where
    I: TraceIdentityProvider + 'static,
    S: TraceMetadataSink + 'static,
{
    let loss = match error {
        TraceRedactionError::HighRisk => ProcessTraceLoss::HighRisk,
        TraceRedactionError::OutputTooLarge | TraceRedactionError::TooManyOutputChunks => {
            ProcessTraceLoss::OutputLimit
        }
        _ => ProcessTraceLoss::Redaction,
    };
    match run.finalize(TraceTerminalOutcome::Failed) {
        Ok(()) => ProcessTraceError::Loss(loss),
        Err(error) => ProcessTraceError::Producer(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::{BTreeMap, BTreeSet},
        path::PathBuf,
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc, Mutex,
        },
    };

    use futures::future::BoxFuture;
    use nwflash_domain::{OperationKind, TraceId};
    use nwflash_protection::{ExactSecretSet, SentinelAttestedTraceUpload};
    use nwflash_windows::process::{
        ObservedProcessOutcome, ProcessFinishMetadata, ProcessObservation, ProcessObserverCallback,
        ProcessObserverLoss, ProcessOutput, ProcessOutputObserver, ProcessOutputStream,
        ProcessStartMetadata, ProcessStdinMode, ProcessStdoutMode, ProcessTermination,
    };
    use serde_json::Value;

    use crate::{
        TraceAuthorization, TraceAuthorizationProvider, TraceIdentityProvider,
        TraceIdentitySnapshot, TraceMetadataSink, TraceProducer, TraceProducerError, TraceRunOpen,
        TraceTerminalOutcome,
    };

    #[derive(Clone)]
    struct FixedIdentity(TraceIdentitySnapshot);

    impl TraceIdentityProvider for FixedIdentity {
        fn verified_identity(&self) -> Option<TraceIdentitySnapshot> {
            Some(self.0.clone())
        }

        fn with_verified_identity<R, F>(
            &self,
            expected: &TraceIdentitySnapshot,
            mutation: F,
        ) -> Option<R>
        where
            F: FnOnce() -> R,
        {
            (&self.0 == expected).then(mutation)
        }
    }

    struct AllowAuthorization;

    impl TraceAuthorizationProvider for AllowAuthorization {
        fn authorize(
            &self,
            _operation: OperationKind,
            _run_id: TraceId,
            _identity: TraceIdentitySnapshot,
        ) -> BoxFuture<'static, Result<TraceAuthorization, TraceProducerError>> {
            Box::pin(async { Ok(TraceAuthorization::Allowed) })
        }
    }

    #[derive(Default)]
    struct RecordingSink {
        uploads: Mutex<Vec<(u64, Vec<u8>)>>,
        attempted_batches: Mutex<Vec<(u64, Vec<TraceId>)>>,
        fail_next_batch: AtomicBool,
        terminals: Mutex<Vec<TraceTerminalOutcome>>,
    }

    impl RecordingSink {
        fn bodies(&self) -> Vec<Value> {
            self.uploads
                .lock()
                .expect("uploads")
                .iter()
                .map(|(_, body)| serde_json::from_slice(body).expect("sealed JSON"))
                .collect()
        }

        fn upload_count(&self) -> usize {
            self.uploads.lock().expect("uploads").len()
        }

        fn terminals(&self) -> Vec<TraceTerminalOutcome> {
            self.terminals.lock().expect("terminals").clone()
        }

        fn fail_next_batch(&self) {
            self.fail_next_batch.store(true, Ordering::SeqCst);
        }

        fn attempted_batches(&self) -> Vec<(u64, Vec<TraceId>)> {
            self.attempted_batches
                .lock()
                .expect("attempted batches")
                .clone()
        }
    }

    impl TraceMetadataSink for RecordingSink {
        fn open_run(&self, _run: &TraceRunOpen) -> Result<(), TraceProducerError> {
            Ok(())
        }

        fn record_authorization(
            &self,
            _run: &TraceRunOpen,
            _authorization: TraceAuthorization,
        ) -> Result<(), TraceProducerError> {
            Ok(())
        }

        fn record_authorization_terminal(
            &self,
            _run: &TraceRunOpen,
            _authorization: TraceAuthorization,
            outcome: TraceTerminalOutcome,
        ) -> Result<(), TraceProducerError> {
            self.terminals.lock().expect("terminals").push(outcome);
            Ok(())
        }

        fn append_upload_attempts(
            &self,
            _run: &TraceRunOpen,
            sequence: u64,
            uploads: &[SentinelAttestedTraceUpload],
        ) -> Result<(), TraceProducerError> {
            self.attempted_batches
                .lock()
                .expect("attempted batches")
                .push((
                    sequence,
                    uploads
                        .iter()
                        .map(SentinelAttestedTraceUpload::upload_id)
                        .collect(),
                ));
            if self.fail_next_batch.swap(false, Ordering::SeqCst) {
                return Err(TraceProducerError::Sink);
            }
            let mut recorded = Vec::with_capacity(uploads.len());
            for upload in uploads {
                let body = upload
                    .to_json_body()
                    .map_err(|_| TraceProducerError::Sink)?;
                recorded.push((sequence, body.to_vec()));
            }
            self.uploads.lock().expect("uploads").extend(recorded);
            Ok(())
        }

        fn terminalize(
            &self,
            _run: &TraceRunOpen,
            outcome: TraceTerminalOutcome,
        ) -> Result<(), TraceProducerError> {
            self.terminals.lock().expect("terminals").push(outcome);
            Ok(())
        }
    }

    async fn run_fixture() -> (
        crate::TraceRunHandle<FixedIdentity, Arc<RecordingSink>>,
        Arc<RecordingSink>,
    ) {
        let identity = FixedIdentity(TraceIdentitySnapshot::new([7; 32], 11, [9; 32]));
        let sink = Arc::new(RecordingSink::default());
        let producer = TraceProducer::new(identity, AllowAuthorization, Arc::clone(&sink));
        let run = producer
            .start(OperationKind::Flashing)
            .await
            .expect("trace run");
        (run, sink)
    }

    fn start_metadata(stdout_mode: ProcessStdoutMode) -> ProcessStartMetadata {
        ProcessStartMetadata {
            program: "fastboot".to_string(),
            args: vec!["flash".to_string(), "boot".to_string()],
            working_directory: Some(PathBuf::from("C:\\bounded-work")),
            stdin_mode: ProcessStdinMode::Inherit,
            stdout_mode,
            elevated: false,
        }
    }

    fn finished(termination: ProcessTermination) -> ProcessFinishMetadata {
        ProcessFinishMetadata {
            exit_code: Some(0),
            termination,
            process_tree_termination_requested: false,
        }
    }

    fn observe_started(collector: &ProcessTraceCollector, metadata: &ProcessStartMetadata) {
        collector
            .observe(ProcessObservation::Started(metadata))
            .expect("started observation");
    }

    fn observe_output(
        collector: &ProcessTraceCollector,
        stream: ProcessOutputStream,
        sequence: u64,
        bytes: &[u8],
    ) {
        collector
            .observe(ProcessObservation::Output {
                stream,
                sequence,
                bytes,
            })
            .expect("output observation");
    }

    fn observe_finished(collector: &ProcessTraceCollector, termination: ProcessTermination) {
        collector
            .observe(ProcessObservation::Finished(finished(termination)))
            .expect("finished observation");
    }

    fn all_json(sink: &RecordingSink) -> String {
        sink.uploads
            .lock()
            .expect("uploads")
            .iter()
            .map(|(_, body)| String::from_utf8(body.clone()).expect("UTF-8 JSON"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn output_by_stream(sink: &RecordingSink) -> BTreeMap<String, String> {
        let mut chunks = BTreeMap::<(String, u64), String>::new();
        for body in sink.bodies() {
            for chunk in body["output_chunks"].as_array().expect("chunks") {
                let stream = chunk["stream"].as_str().expect("stream").to_string();
                let index = chunk["chunk_index"].as_u64().expect("chunk index");
                chunks.insert(
                    (stream, index),
                    chunk["text"].as_str().expect("text").to_string(),
                );
            }
        }
        let mut streams = BTreeMap::new();
        for ((stream, _), text) in chunks {
            streams
                .entry(stream)
                .or_insert_with(String::new)
                .push_str(&text);
        }
        streams
    }

    #[tokio::test]
    async fn split_credentials_are_scanned_after_each_complete_logical_stream() {
        let (run, sink) = run_fixture().await;
        let exact = "exact-secret-value";
        let collector = ProcessTraceCollector::try_new(
            "flash-command",
            1_000,
            ExactSecretSet::try_new([exact.as_bytes()]).expect("secret set"),
        )
        .expect("collector");
        let mut metadata = start_metadata(ProcessStdoutMode::TextPipe);
        metadata.program = format!("runner-{exact}");
        metadata.args = vec!["--api-key".to_string(), exact.to_string()];
        metadata.working_directory = Some(PathBuf::from(format!("C:\\{exact}")));
        observe_started(&collector, &metadata);
        observe_output(
            &collector,
            ProcessOutputStream::Stdout,
            0,
            b"Authorization: Bea",
        );
        observe_output(
            &collector,
            ProcessOutputStream::Stdout,
            1,
            b"rer bearer-value-123\n-----BEGIN PRIVATE ",
        );
        observe_output(
            &collector,
            ProcessOutputStream::Stdout,
            2,
            b"KEY-----\npem-private-value\n-----END PRIVATE KEY-----\n",
        );
        observe_output(
            &collector,
            ProcessOutputStream::Stderr,
            0,
            b"prefix exact-se",
        );
        observe_output(
            &collector,
            ProcessOutputStream::Stderr,
            1,
            b"cret-value suffix\n",
        );
        observe_finished(&collector, ProcessTermination::Completed);

        let committed = collector
            .finish_into_run(&run, &[], 1_125)
            .expect("sealed append");

        assert_eq!(committed.event_ids().len(), 2);
        let json = all_json(&sink);
        for raw in [
            "bearer-value-123",
            "pem-private-value",
            "exact-secret-value",
            "-----BEGIN PRIVATE KEY-----",
        ] {
            assert!(!json.contains(raw), "raw sentinel leaked: {raw}");
        }
        assert!(!json.contains("\"env\""));
        assert!(!json.contains("environment"));
        assert!(json.contains("CREDENTIAL_REMOVED:PRIVATE_KEY"));
        assert!(json.contains("REDACTED"));
    }

    #[tokio::test]
    async fn stdout_and_stderr_retain_independent_callback_order() {
        let (run, sink) = run_fixture().await;
        let collector =
            ProcessTraceCollector::try_new("ordered-command", 2_000, ExactSecretSet::empty())
                .expect("collector");
        let metadata = start_metadata(ProcessStdoutMode::TextPipe);
        observe_started(&collector, &metadata);
        observe_output(&collector, ProcessOutputStream::Stdout, 0, b"stdout-1|");
        observe_output(&collector, ProcessOutputStream::Stderr, 0, b"stderr-1|");
        observe_output(&collector, ProcessOutputStream::Stdout, 1, b"stdout-2");
        observe_output(&collector, ProcessOutputStream::Stderr, 1, b"stderr-2");
        observe_finished(&collector, ProcessTermination::Completed);

        let committed = collector
            .finish_into_run(&run, &[], 2_010)
            .expect("sealed append");

        assert_eq!(committed.event_ids().len(), 2);
        assert_eq!(run.next_sequence(), 3);
        let streams = output_by_stream(&sink);
        assert_eq!(
            streams.get("stdout").map(String::as_str),
            Some("stdout-1|stdout-2")
        );
        assert_eq!(
            streams.get("stderr").map(String::as_str),
            Some("stderr-1|stderr-2")
        );
    }

    #[tokio::test]
    async fn binary_file_stdout_never_enters_the_trace_collector() {
        let (run, sink) = run_fixture().await;
        let collector =
            ProcessTraceCollector::try_new("binary-output-command", 3_000, ExactSecretSet::empty())
                .expect("collector");
        let metadata = start_metadata(ProcessStdoutMode::BinaryFile);
        observe_started(&collector, &metadata);
        observe_output(
            &collector,
            ProcessOutputStream::Stdout,
            0,
            b"binary-stdout-secret-must-not-be-captured",
        );
        observe_output(&collector, ProcessOutputStream::Stderr, 0, b"safe stderr");
        observe_finished(&collector, ProcessTermination::Completed);

        let committed = collector
            .finish_into_run(&run, &[], 3_010)
            .expect("sealed append");

        assert_eq!(committed.event_ids().len(), 1);
        let json = all_json(&sink);
        assert!(!json.contains("binary-stdout-secret-must-not-be-captured"));
        assert!(json.contains("safe stderr"));
        assert!(!json.contains("\"stream\":\"stdout\""));
    }

    #[tokio::test]
    async fn observer_loss_and_output_limit_fail_the_run_without_uploading_raw_data() {
        let (run, sink) = run_fixture().await;
        let collector =
            ProcessTraceCollector::try_new("loss-command", 4_000, ExactSecretSet::empty())
                .expect("collector");
        let metadata = start_metadata(ProcessStdoutMode::TextPipe);
        observe_started(&collector, &metadata);
        observe_output(
            &collector,
            ProcessOutputStream::Stdout,
            0,
            b"never persisted",
        );
        observe_finished(&collector, ProcessTermination::Completed);
        let losses = [ProcessObserverLoss {
            callback: ProcessObserverCallback::Output,
            stream: Some(ProcessOutputStream::Stdout),
            sequence: Some(1),
        }];

        let outcome = ObservedProcessOutcome {
            result: Ok(ProcessOutput {
                exit_code: 0,
                stdout: String::new(),
                stderr: String::new(),
            }),
            observer_losses: losses.to_vec(),
        };
        assert_eq!(
            collector.finish_observed_into_run(&run, outcome, 4_010),
            Err(ProcessTraceError::Loss(ProcessTraceLoss::Observer))
        );
        assert!(run.is_terminal());
        assert_eq!(sink.upload_count(), 0);
        assert_eq!(sink.terminals(), vec![TraceTerminalOutcome::Failed]);

        let (run, sink) = run_fixture().await;
        let collector = ProcessTraceCollector::try_new_with_output_limit(
            "bounded-command",
            4_100,
            ExactSecretSet::empty(),
            8,
        )
        .expect("collector");
        observe_started(&collector, &metadata);
        assert!(collector
            .observe(ProcessObservation::Output {
                stream: ProcessOutputStream::Stdout,
                sequence: 0,
                bytes: b"nine-byte",
            })
            .is_err());
        observe_finished(&collector, ProcessTermination::Completed);
        assert_eq!(
            collector.finish_into_run(&run, &[], 4_110),
            Err(ProcessTraceError::Loss(ProcessTraceLoss::OutputLimit))
        );
        assert!(run.is_terminal());
        assert_eq!(sink.upload_count(), 0);
    }

    #[tokio::test]
    async fn termination_unconfirmed_and_high_risk_output_cannot_finalize_successfully() {
        let (run, sink) = run_fixture().await;
        let collector =
            ProcessTraceCollector::try_new("uncertain-command", 5_000, ExactSecretSet::empty())
                .expect("collector");
        let metadata = start_metadata(ProcessStdoutMode::TextPipe);
        observe_started(&collector, &metadata);
        observe_finished(&collector, ProcessTermination::TerminationUnconfirmed);
        assert_eq!(
            collector.finish_into_run(&run, &[], 5_010),
            Err(ProcessTraceError::Loss(
                ProcessTraceLoss::TerminationUnconfirmed
            ))
        );
        assert!(run.is_terminal());
        assert_eq!(sink.upload_count(), 0);

        let (run, sink) = run_fixture().await;
        let collector =
            ProcessTraceCollector::try_new("high-risk-command", 5_100, ExactSecretSet::empty())
                .expect("collector");
        observe_started(&collector, &metadata);
        observe_output(&collector, ProcessOutputStream::Stderr, 0, &[0xff, 0xfe]);
        observe_finished(&collector, ProcessTermination::Completed);
        assert_eq!(
            collector.finish_into_run(&run, &[], 5_110),
            Err(ProcessTraceError::Loss(ProcessTraceLoss::HighRisk))
        );
        assert!(run.is_terminal());
        assert_eq!(sink.upload_count(), 0);
    }

    #[tokio::test]
    async fn output_read_failure_is_terminal_loss_with_zero_uploads() {
        let (run, sink) = run_fixture().await;
        let collector =
            ProcessTraceCollector::try_new("read-failed", 5_200, ExactSecretSet::empty())
                .expect("collector");
        let metadata = start_metadata(ProcessStdoutMode::TextPipe);
        observe_started(&collector, &metadata);
        observe_output(
            &collector,
            ProcessOutputStream::Stdout,
            0,
            b"must never be persisted",
        );
        observe_finished(&collector, ProcessTermination::OutputReadFailed);
        let outcome = ObservedProcessOutcome {
            result: Ok(ProcessOutput {
                exit_code: 1,
                stdout: "legacy raw stdout".to_string(),
                stderr: "legacy raw stderr".to_string(),
            }),
            observer_losses: Vec::new(),
        };

        assert_eq!(
            collector.finish_observed_into_run(&run, outcome, 5_210),
            Err(ProcessTraceError::Loss(ProcessTraceLoss::IncompleteOutput))
        );
        assert_eq!(sink.upload_count(), 0);
        assert_eq!(sink.terminals(), vec![TraceTerminalOutcome::Failed]);
    }

    #[tokio::test]
    async fn bounded_attempts_use_fresh_upload_ids_and_one_stable_event_id() {
        let (run, sink) = run_fixture().await;
        let collector =
            ProcessTraceCollector::try_new("large-command", 6_000, ExactSecretSet::empty())
                .expect("collector");
        let metadata = start_metadata(ProcessStdoutMode::TextPipe);
        observe_started(&collector, &metadata);
        let output = format!("{}\n", "x".repeat(1_023))
            .repeat(1_200)
            .into_bytes();
        for (sequence, bytes) in output.chunks(16 * 1024).enumerate() {
            observe_output(
                &collector,
                ProcessOutputStream::Stdout,
                sequence as u64,
                bytes,
            );
        }
        observe_finished(&collector, ProcessTermination::Completed);

        let committed = collector
            .finish_into_run(&run, &[], 6_010)
            .expect("sealed append");
        let bodies = sink.bodies();
        assert!(bodies.len() > 1, "fixture must force bounded attempts");
        assert_eq!(committed.attempt_count(), bodies.len());
        assert_eq!(
            run.next_sequence(),
            2,
            "one event may span attempts but consumes one event sequence"
        );

        let upload_ids = bodies
            .iter()
            .map(|body| body["upload_id"].as_str().expect("upload ID"))
            .collect::<BTreeSet<_>>();
        assert_eq!(upload_ids.len(), bodies.len());
        let event_id = committed.event_ids()[0].to_string();
        for body in bodies {
            for event in body["events"].as_array().expect("events") {
                assert_eq!(event["event_id"].as_str(), Some(event_id.as_str()));
            }
            for chunk in body["output_chunks"].as_array().expect("chunks") {
                assert_eq!(chunk["event_id"].as_str(), Some(event_id.as_str()));
            }
        }
    }

    #[tokio::test]
    async fn failed_atomic_batch_retries_the_same_ordered_upload_ids_and_one_sequence() {
        let (run, sink) = run_fixture().await;
        let collector =
            ProcessTraceCollector::try_new("retry-command", 6_100, ExactSecretSet::empty())
                .expect("collector");
        let metadata = start_metadata(ProcessStdoutMode::TextPipe);
        observe_started(&collector, &metadata);
        let output = format!("{}\n", "r".repeat(1_023))
            .repeat(1_200)
            .into_bytes();
        for (sequence, bytes) in output.chunks(16 * 1024).enumerate() {
            observe_output(
                &collector,
                ProcessOutputStream::Stdout,
                sequence as u64,
                bytes,
            );
        }
        observe_finished(&collector, ProcessTermination::Completed);
        sink.fail_next_batch();
        let outcome = ObservedProcessOutcome {
            result: Ok(ProcessOutput {
                exit_code: 0,
                stdout: String::new(),
                stderr: String::new(),
            }),
            observer_losses: Vec::new(),
        };

        assert_eq!(
            collector.finish_observed_into_run(&run, outcome, 6_110),
            Err(ProcessTraceError::Producer(TraceProducerError::Sink))
        );
        assert_eq!(sink.upload_count(), 0);
        assert_eq!(run.next_sequence(), 2);
        let first_attempt = sink.attempted_batches();
        assert_eq!(first_attempt.len(), 1);
        assert!(first_attempt[0].1.len() > 1, "fixture must span attempts");

        let committed = collector
            .retry_pending_into_run(&run)
            .expect("same batch can retry");
        let attempted = sink.attempted_batches();
        assert_eq!(attempted.len(), 2);
        assert_eq!(attempted[0], attempted[1]);
        assert_eq!(attempted[0].0, 1);
        assert_eq!(committed.upload_ids(), attempted[0].1.as_slice());
        assert_eq!(run.next_sequence(), 2);
        assert_eq!(
            collector.retry_pending_into_run(&run),
            Err(ProcessTraceError::AlreadyConsumed)
        );
    }

    #[tokio::test]
    async fn debug_output_exposes_only_counts_and_ids() {
        let (run, sink) = run_fixture().await;
        let secret = "debug-output-secret";
        let collector = ProcessTraceCollector::try_new(
            secret,
            7_000,
            ExactSecretSet::try_new([secret.as_bytes()]).expect("secret set"),
        )
        .expect("collector");
        let mut metadata = start_metadata(ProcessStdoutMode::TextPipe);
        metadata.args.push(secret.to_string());
        observe_started(&collector, &metadata);
        observe_output(
            &collector,
            ProcessOutputStream::Stdout,
            0,
            secret.as_bytes(),
        );
        let debug = format!("{collector:?}");
        assert!(!debug.contains(secret));
        observe_finished(&collector, ProcessTermination::Completed);
        let committed = collector
            .finish_into_run(&run, &[], 7_010)
            .expect("sealed append");
        assert!(!format!("{committed:?}").contains(secret));
        assert!(!all_json(&sink).contains(secret));
    }

    #[tokio::test]
    async fn public_finish_requires_the_observed_outcome_and_ignores_legacy_raw_result_text() {
        let (run, sink) = run_fixture().await;
        let collector =
            ProcessTraceCollector::try_new("outcome-command", 8_000, ExactSecretSet::empty())
                .expect("collector");
        let metadata = start_metadata(ProcessStdoutMode::TextPipe);
        observe_started(&collector, &metadata);
        observe_output(
            &collector,
            ProcessOutputStream::Stdout,
            0,
            b"safe callback text",
        );
        observe_finished(&collector, ProcessTermination::Completed);
        let outcome = ObservedProcessOutcome {
            result: Ok(ProcessOutput {
                exit_code: 0,
                stdout: "legacy-raw-result-must-not-enter-wire".to_string(),
                stderr: String::new(),
            }),
            observer_losses: Vec::new(),
        };

        collector
            .finish_observed_into_run(&run, outcome, 8_010)
            .expect("sealed append");

        let json = all_json(&sink);
        assert!(json.contains("safe callback text"));
        assert!(!json.contains("legacy-raw-result-must-not-enter-wire"));
    }
}
