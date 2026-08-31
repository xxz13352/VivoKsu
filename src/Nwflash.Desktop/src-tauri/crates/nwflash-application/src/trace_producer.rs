use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use futures::future::BoxFuture;

use nwflash_domain::{
    OperationKind, TraceId, TraceOutputStreamV2, TRACE_RUN_MAX_EVENT_STORAGE_BYTES,
    TRACE_UPLOAD_MAX_OUTPUT_CHUNKS,
};
use nwflash_protection::{SealedTraceUpload, SentinelAttestedTraceUpload};

const MAX_TRACE_EVENT_OUTPUT_CHUNKS: usize = TRACE_UPLOAD_MAX_OUTPUT_CHUNKS * 2;
const MAX_TRACE_EVENT_UPLOAD_ATTEMPTS: usize = MAX_TRACE_EVENT_OUTPUT_CHUNKS + 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceAuthorization {
    Allowed,
    AdvisoryAllowed,
    Denied,
}

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum TraceProducerError {
    #[error("trace producer requires a verified session identity")]
    Unauthenticated,
    #[error("operation kind is not traceable")]
    InvalidOperationKind,
    #[error("remote authorization denied")]
    Denied,
    #[error("remote authorization failed")]
    AuthorizationFailed,
    #[error("session identity epoch is stale")]
    StaleIdentity,
    #[error("trace run is already terminal")]
    AlreadyTerminal,
    #[error("trace sequence must be contiguous")]
    SequenceGap,
    #[error("trace sequence limit reached")]
    SequenceLimit,
    #[error("trace upload batch is empty")]
    EmptyUploadBatch,
    #[error("trace upload attempts must belong to one event")]
    MixedEventAttempts,
    #[error("trace upload attempts require exactly one event manifest")]
    UnboundEventAttempts,
    #[error("trace upload event manifest must be the first and only manifest")]
    EventManifestOrder,
    #[error("trace upload event manifest does not match the run reservation")]
    EventBindingMismatch,
    #[error("trace event batch cannot contain run-level records")]
    UnexpectedRunMetadata,
    #[error("trace upload attempts do not contain the complete declared output")]
    IncompleteEventAttempts,
    #[error("trace upload batch contains a duplicate upload id")]
    DuplicateUploadAttempt,
    #[error("trace upload attempts are not in contiguous stream order")]
    UploadAttemptOrder,
    #[error("trace upload batch exceeds the per-event bound")]
    UploadBatchLimit,
    #[error("trace sink rejected the mutation")]
    Sink,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceIdentitySnapshot {
    username_hash: [u8; 32],
    epoch: u64,
    build_hash: [u8; 32],
}

impl TraceIdentitySnapshot {
    pub const fn new(username_hash: [u8; 32], epoch: u64, build_hash: [u8; 32]) -> Self {
        Self {
            username_hash,
            epoch,
            build_hash,
        }
    }

    pub const fn epoch(&self) -> u64 {
        self.epoch
    }

    pub const fn username_hash(&self) -> &[u8; 32] {
        &self.username_hash
    }

    pub const fn build_hash(&self) -> &[u8; 32] {
        &self.build_hash
    }
}

/// Intentionally statically dispatched: the generic callback is the identity
/// lease boundary that keeps a generation switch from racing a sink mutation.
/// Adapters should wrap a concrete provider in `Arc` rather than erase it to a
/// trait object and weaken this boundary.
pub trait TraceIdentityProvider: Send + Sync {
    fn verified_identity(&self) -> Option<TraceIdentitySnapshot>;

    /// Runs `mutation` only while the captured identity is still current.
    /// Implementations must serialize identity replacement against this call;
    /// a check followed by an unlocked callback is not sufficient.
    fn with_verified_identity<R, F>(
        &self,
        expected: &TraceIdentitySnapshot,
        mutation: F,
    ) -> Option<R>
    where
        F: FnOnce() -> R;
}

impl<T> TraceIdentityProvider for Arc<T>
where
    T: TraceIdentityProvider + ?Sized,
{
    fn verified_identity(&self) -> Option<TraceIdentitySnapshot> {
        (**self).verified_identity()
    }

    fn with_verified_identity<R, F>(
        &self,
        expected: &TraceIdentitySnapshot,
        mutation: F,
    ) -> Option<R>
    where
        F: FnOnce() -> R,
    {
        (**self).with_verified_identity(expected, mutation)
    }
}

pub trait TraceAuthorizationProvider: Send + Sync {
    /// Authorizes only the supplied captured identity. Implementations must not
    /// substitute an ambient session or token that may belong to a newer login.
    fn authorize(
        &self,
        operation: OperationKind,
        run_id: TraceId,
        identity: TraceIdentitySnapshot,
    ) -> BoxFuture<'static, Result<TraceAuthorization, TraceProducerError>>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceTerminalOutcome {
    Success,
    Failed,
    Canceled,
    Denied,
    Aborted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TraceRunOpen {
    pub run_id: TraceId,
    pub operation: OperationKind,
    pub owner_username_hash: [u8; 32],
    pub identity_epoch: u64,
    pub build_hash: [u8; 32],
}

pub trait TraceMetadataSink: Send + Sync {
    /// Every mutation carries the immutable owner context originally persisted
    /// by `open_run`. Implementations must bind writes to this context instead
    /// of resolving an ambient/current user.
    fn open_run(&self, run: &TraceRunOpen) -> Result<(), TraceProducerError>;
    fn record_authorization(
        &self,
        run: &TraceRunOpen,
        authorization: TraceAuthorization,
    ) -> Result<(), TraceProducerError>;
    /// Atomically records a terminal authorization decision. Before returning,
    /// including on a diagnostic error, the sink must either persist the
    /// terminal state or durably enqueue its recovery; a bare open run is not a
    /// valid result.
    fn record_authorization_terminal(
        &self,
        run: &TraceRunOpen,
        authorization: TraceAuthorization,
        outcome: TraceTerminalOutcome,
    ) -> Result<(), TraceProducerError>;
    /// Raw sealed uploads cannot cross the producer-to-sink capability boundary.
    ///
    /// ```compile_fail
    /// use nwflash_application::{TraceMetadataSink, TraceRunOpen};
    /// use nwflash_protection::SealedTraceUpload;
    ///
    /// fn raw_uploads_cannot_reach_a_sink<S: TraceMetadataSink>(
    ///     sink: &S,
    ///     run: &TraceRunOpen,
    ///     uploads: &[SealedTraceUpload],
    /// ) {
    ///     sink.append_upload_attempts(run, 1, uploads).unwrap();
    /// }
    /// ```
    ///
    /// Atomically records the ordered attempts for one logical event sequence.
    /// Implementations must leave no successful attempt side effects when this
    /// method returns an error, so the producer can keep the reservation open.
    fn append_upload_attempts(
        &self,
        run: &TraceRunOpen,
        sequence: u64,
        uploads: &[SentinelAttestedTraceUpload],
    ) -> Result<(), TraceProducerError>;
    /// Before returning, including on a diagnostic error, the sink must either
    /// persist this terminal state or durably enqueue its recovery. This keeps
    /// authorization races and cleanup failures from leaving a bare open run.
    fn terminalize(
        &self,
        run: &TraceRunOpen,
        outcome: TraceTerminalOutcome,
    ) -> Result<(), TraceProducerError>;
}

impl<T> TraceMetadataSink for Arc<T>
where
    T: TraceMetadataSink + ?Sized,
{
    fn open_run(&self, run: &TraceRunOpen) -> Result<(), TraceProducerError> {
        (**self).open_run(run)
    }

    fn record_authorization(
        &self,
        run: &TraceRunOpen,
        authorization: TraceAuthorization,
    ) -> Result<(), TraceProducerError> {
        (**self).record_authorization(run, authorization)
    }

    fn record_authorization_terminal(
        &self,
        run: &TraceRunOpen,
        authorization: TraceAuthorization,
        outcome: TraceTerminalOutcome,
    ) -> Result<(), TraceProducerError> {
        (**self).record_authorization_terminal(run, authorization, outcome)
    }

    fn append_upload_attempts(
        &self,
        run: &TraceRunOpen,
        sequence: u64,
        uploads: &[SentinelAttestedTraceUpload],
    ) -> Result<(), TraceProducerError> {
        (**self).append_upload_attempts(run, sequence, uploads)
    }

    fn terminalize(
        &self,
        run: &TraceRunOpen,
        outcome: TraceTerminalOutcome,
    ) -> Result<(), TraceProducerError> {
        (**self).terminalize(run, outcome)
    }
}

#[derive(Clone)]
pub struct TraceProducer<I, A, S> {
    identity: Arc<I>,
    authorization: Arc<A>,
    sink: Arc<S>,
}

pub struct TraceRunHandle<I, S> {
    run: TraceRunOpen,
    captured_identity: TraceIdentitySnapshot,
    identity: Arc<I>,
    sink: Arc<S>,
    authorization: TraceAuthorization,
    state: Mutex<TraceRunState>,
}

struct TraceRunState {
    next_sequence: u64,
    terminal: bool,
    reserved_sequences: BTreeSet<u64>,
}

impl<I, A, S> TraceProducer<I, A, S>
where
    I: TraceIdentityProvider + 'static,
    A: TraceAuthorizationProvider + 'static,
    S: TraceMetadataSink + 'static,
{
    pub fn new(identity: I, authorization: A, sink: S) -> Self {
        Self {
            identity: Arc::new(identity),
            authorization: Arc::new(authorization),
            sink: Arc::new(sink),
        }
    }

    pub async fn start(
        &self,
        operation: OperationKind,
    ) -> Result<TraceRunHandle<I, S>, TraceProducerError> {
        if !is_trace_operation(operation) {
            return Err(TraceProducerError::InvalidOperationKind);
        }
        let Some(identity) = self.identity.verified_identity() else {
            return Err(TraceProducerError::Unauthenticated);
        };
        let run_id = TraceId::try_new_v7().map_err(|_| TraceProducerError::Sink)?;
        let run = TraceRunOpen {
            run_id,
            operation,
            owner_username_hash: *identity.username_hash(),
            identity_epoch: identity.epoch(),
            build_hash: *identity.build_hash(),
        };
        self.identity
            .with_verified_identity(&identity, || self.sink.open_run(&run))
            .ok_or(TraceProducerError::StaleIdentity)?
            .map_err(|_| TraceProducerError::Sink)?;

        let authorization = match self
            .authorization
            .authorize(operation, run_id, identity.clone())
            .await
        {
            Ok(value) => value,
            Err(_) => {
                let settlement = self.identity.with_verified_identity(&identity, || {
                    self.sink.record_authorization_terminal(
                        &run,
                        TraceAuthorization::Denied,
                        TraceTerminalOutcome::Denied,
                    )
                });
                return match settlement {
                    Some(Ok(())) => Err(TraceProducerError::AuthorizationFailed),
                    Some(Err(_)) => Err(TraceProducerError::Sink),
                    None => {
                        if self
                            .sink
                            .terminalize(&run, TraceTerminalOutcome::Aborted)
                            .is_err()
                        {
                            Err(TraceProducerError::Sink)
                        } else {
                            Err(TraceProducerError::StaleIdentity)
                        }
                    }
                };
            }
        };
        let settlement = self.identity.with_verified_identity(&identity, || {
            if authorization == TraceAuthorization::Denied {
                return self.sink.record_authorization_terminal(
                    &run,
                    authorization,
                    TraceTerminalOutcome::Denied,
                );
            }
            let authorization_result = self.sink.record_authorization(&run, authorization);
            if authorization_result.is_err()
                && self
                    .sink
                    .terminalize(&run, TraceTerminalOutcome::Aborted)
                    .is_err()
            {
                return Err(TraceProducerError::Sink);
            }
            authorization_result
        });
        let Some(settlement_result) = settlement else {
            self.sink
                .terminalize(&run, TraceTerminalOutcome::Aborted)
                .map_err(|_| TraceProducerError::Sink)?;
            return Err(TraceProducerError::StaleIdentity);
        };
        if settlement_result.is_err() {
            return Err(TraceProducerError::Sink);
        }
        if authorization == TraceAuthorization::Denied {
            return Err(TraceProducerError::Denied);
        }
        let handle = TraceRunHandle {
            run,
            captured_identity: identity,
            identity: self.identity.clone(),
            sink: self.sink.clone(),
            authorization,
            state: Mutex::new(TraceRunState {
                next_sequence: 1,
                terminal: false,
                reserved_sequences: BTreeSet::new(),
            }),
        };
        Ok(handle)
    }
}

impl<I, S> TraceRunHandle<I, S>
where
    I: TraceIdentityProvider + 'static,
    S: TraceMetadataSink + 'static,
{
    pub const fn run_id(&self) -> TraceId {
        self.run.run_id
    }

    pub const fn authorization(&self) -> TraceAuthorization {
        self.authorization
    }

    pub const fn operation(&self) -> OperationKind {
        self.run.operation
    }

    pub fn is_terminal(&self) -> bool {
        self.state.lock().expect("trace state lock").terminal
    }

    pub fn next_sequence(&self) -> u64 {
        self.state.lock().expect("trace state lock").next_sequence
    }

    pub fn reserve_sequence(&self, sequence: u64) -> Result<u64, TraceProducerError> {
        self.identity
            .with_verified_identity(&self.captured_identity, || {
                let mut state = self.state.lock().expect("trace state lock");
                if state.terminal {
                    return Err(TraceProducerError::AlreadyTerminal);
                }
                if sequence != state.next_sequence {
                    return Err(TraceProducerError::SequenceGap);
                }
                if sequence > nwflash_domain::TRACE_RUN_MAX_EVENTS as u64 {
                    return Err(TraceProducerError::SequenceLimit);
                }
                state.next_sequence = state.next_sequence.saturating_add(1);
                state.reserved_sequences.insert(sequence);
                Ok(sequence)
            })
            .ok_or(TraceProducerError::StaleIdentity)?
    }

    pub fn append_upload(
        &self,
        sequence: u64,
        upload: &SentinelAttestedTraceUpload,
    ) -> Result<(), TraceProducerError> {
        self.append_upload_attempts(sequence, std::slice::from_ref(upload))
    }

    pub fn append_upload_attempts(
        &self,
        sequence: u64,
        uploads: &[SentinelAttestedTraceUpload],
    ) -> Result<(), TraceProducerError> {
        if sequence > nwflash_domain::TRACE_RUN_MAX_EVENTS as u64 {
            return Err(TraceProducerError::SequenceLimit);
        }
        self.validate_upload_attempts(sequence, uploads)?;
        self.identity
            .with_verified_identity(&self.captured_identity, || {
                let mut state = self.state.lock().expect("trace state lock");
                if state.terminal {
                    return Err(TraceProducerError::AlreadyTerminal);
                }
                if let Some(expected_reserved) = state.reserved_sequences.first().copied() {
                    if sequence != expected_reserved {
                        return Err(TraceProducerError::SequenceGap);
                    }
                }
                let reserved = state.reserved_sequences.contains(&sequence);
                if !reserved {
                    if sequence != state.next_sequence {
                        return Err(TraceProducerError::SequenceGap);
                    }
                    if sequence > nwflash_domain::TRACE_RUN_MAX_EVENTS as u64 {
                        return Err(TraceProducerError::SequenceLimit);
                    }
                }
                self.sink
                    .append_upload_attempts(&self.run, sequence, uploads)
                    .map_err(|_| TraceProducerError::Sink)?;
                if reserved {
                    state.reserved_sequences.remove(&sequence);
                } else {
                    state.next_sequence = state.next_sequence.saturating_add(1);
                }
                Ok(())
            })
            .ok_or(TraceProducerError::StaleIdentity)?
    }

    fn validate_upload_attempts(
        &self,
        sequence: u64,
        uploads: &[SentinelAttestedTraceUpload],
    ) -> Result<(), TraceProducerError> {
        if uploads.is_empty() {
            return Err(TraceProducerError::EmptyUploadBatch);
        }
        if uploads.len() > MAX_TRACE_EVENT_UPLOAD_ATTEMPTS {
            return Err(TraceProducerError::UploadBatchLimit);
        }
        let upload_ids = uploads
            .iter()
            .map(SentinelAttestedTraceUpload::upload_id)
            .collect::<BTreeSet<_>>();
        if upload_ids.len() != uploads.len() {
            return Err(TraceProducerError::DuplicateUploadAttempt);
        }
        if uploads.iter().any(|upload| upload.run_count() != 0) {
            return Err(TraceProducerError::UnexpectedRunMetadata);
        }
        let event_ids = uploads
            .iter()
            .flat_map(SentinelAttestedTraceUpload::event_ids)
            .collect::<BTreeSet<_>>();
        if event_ids.len() != 1 {
            return Err(TraceProducerError::MixedEventAttempts);
        }
        let first_bindings = uploads[0].event_bindings();
        let later_has_manifest = uploads[1..]
            .iter()
            .any(|upload| !upload.event_bindings().is_empty());
        if first_bindings.is_empty() && !later_has_manifest {
            return Err(TraceProducerError::UnboundEventAttempts);
        }
        if first_bindings.len() != 1 || later_has_manifest {
            return Err(TraceProducerError::EventManifestOrder);
        }
        let binding = first_bindings[0];
        if binding.event_id != *event_ids.first().expect("exactly one event id")
            || binding.run_id != self.run.run_id
            || binding.sequence != sequence
        {
            return Err(TraceProducerError::EventBindingMismatch);
        }

        let mut next_stdout = 0_u64;
        let mut next_stderr = 0_u64;
        let mut chunk_count = 0_usize;
        let mut stored_bytes = 0_u64;
        for upload in uploads {
            for chunk in upload.output_chunks() {
                let next = match chunk.stream() {
                    TraceOutputStreamV2::Stdout => &mut next_stdout,
                    TraceOutputStreamV2::Stderr => &mut next_stderr,
                };
                if chunk.chunk_index() != *next {
                    return Err(TraceProducerError::UploadAttemptOrder);
                }
                *next = next.saturating_add(1);
                chunk_count = chunk_count.saturating_add(1);
                stored_bytes = stored_bytes.saturating_add(chunk.byte_count());
            }
        }
        if chunk_count > MAX_TRACE_EVENT_OUTPUT_CHUNKS
            || stored_bytes > TRACE_RUN_MAX_EVENT_STORAGE_BYTES as u64
        {
            return Err(TraceProducerError::UploadBatchLimit);
        }
        if next_stdout != binding.stdout_chunks || next_stderr != binding.stderr_chunks {
            return Err(TraceProducerError::IncompleteEventAttempts);
        }
        Ok(())
    }

    pub fn finalize(&self, outcome: TraceTerminalOutcome) -> Result<(), TraceProducerError> {
        self.identity
            .with_verified_identity(&self.captured_identity, || {
                let mut state = self.state.lock().expect("trace state lock");
                if state.terminal {
                    return Err(TraceProducerError::AlreadyTerminal);
                }
                if !state.reserved_sequences.is_empty() {
                    return Err(TraceProducerError::SequenceGap);
                }
                self.sink
                    .terminalize(&self.run, outcome)
                    .map_err(|_| TraceProducerError::Sink)?;
                state.terminal = true;
                Ok(())
            })
            .ok_or(TraceProducerError::StaleIdentity)?
    }
}

fn is_trace_operation(operation: OperationKind) -> bool {
    match operation {
        OperationKind::Discovering
        | OperationKind::Rebooting
        | OperationKind::Installing
        | OperationKind::Transferring
        | OperationKind::Hashing
        | OperationKind::Flashing
        | OperationKind::Mirroring => true,
        OperationKind::Idle
        | OperationKind::Completed
        | OperationKind::Canceled
        | OperationKind::Failed => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Cursor, sync::Mutex};

    use nwflash_domain::{
        TraceEventKindV2, TraceEventStatusV2, TraceOutcomeV2, TraceOutputStreamV2,
    };
    use nwflash_protection::{
        ExactSecretSet, RedactedTraceEvent, RedactedTraceRun, TraceEventText, TraceOutputSession,
        TraceRunText,
    };

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct SinkOwner {
        run_id: TraceId,
        identity_epoch: u64,
        owner_username_hash: [u8; 32],
        build_hash: [u8; 32],
    }

    impl From<&TraceRunOpen> for SinkOwner {
        fn from(run: &TraceRunOpen) -> Self {
            Self {
                run_id: run.run_id,
                identity_epoch: run.identity_epoch,
                owner_username_hash: run.owner_username_hash,
                build_hash: run.build_hash,
            }
        }
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum SinkCall {
        Open(SinkOwner, OperationKind),
        Authorization(SinkOwner, TraceAuthorization),
        UploadBatch(SinkOwner, u64, usize),
        Terminal(SinkOwner, TraceTerminalOutcome),
    }

    #[derive(Default)]
    struct RecordingSink {
        calls: Mutex<Vec<SinkCall>>,
    }

    #[derive(Clone)]
    struct FixedIdentity {
        snapshot: Arc<Mutex<Option<TraceIdentitySnapshot>>>,
    }

    struct SwitchAfterSnapshotIdentity {
        snapshot: Mutex<Option<TraceIdentitySnapshot>>,
        replacement: Option<TraceIdentitySnapshot>,
    }

    struct FixedAuthorizer {
        decision: Result<TraceAuthorization, TraceProducerError>,
    }

    struct BlockingAuthorizer {
        started: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
        observed: Arc<Mutex<Vec<TraceIdentitySnapshot>>>,
        decision: Result<TraceAuthorization, TraceProducerError>,
    }

    struct FailingSink {
        calls: Mutex<Vec<SinkCall>>,
        open_runs: Mutex<BTreeSet<TraceId>>,
        terminal_runs: Mutex<std::collections::BTreeMap<TraceId, TraceTerminalOutcome>>,
        fail_authorization: bool,
        fail_terminal: bool,
    }

    struct FailOnceBatchSink {
        calls: Mutex<Vec<SinkCall>>,
        attempted_upload_ids: Mutex<Vec<Vec<TraceId>>>,
        fail_next: std::sync::atomic::AtomicBool,
    }

    impl TraceMetadataSink for RecordingSink {
        fn open_run(&self, run: &TraceRunOpen) -> Result<(), TraceProducerError> {
            self.calls
                .lock()
                .unwrap()
                .push(SinkCall::Open(SinkOwner::from(run), run.operation));
            Ok(())
        }

        fn record_authorization(
            &self,
            run: &TraceRunOpen,
            authorization: TraceAuthorization,
        ) -> Result<(), TraceProducerError> {
            self.calls
                .lock()
                .unwrap()
                .push(SinkCall::Authorization(SinkOwner::from(run), authorization));
            Ok(())
        }

        fn record_authorization_terminal(
            &self,
            run: &TraceRunOpen,
            authorization: TraceAuthorization,
            outcome: TraceTerminalOutcome,
        ) -> Result<(), TraceProducerError> {
            let owner = SinkOwner::from(run);
            let mut calls = self.calls.lock().unwrap();
            calls.push(SinkCall::Authorization(owner.clone(), authorization));
            calls.push(SinkCall::Terminal(owner, outcome));
            Ok(())
        }

        fn append_upload_attempts(
            &self,
            run: &TraceRunOpen,
            sequence: u64,
            uploads: &[SentinelAttestedTraceUpload],
        ) -> Result<(), TraceProducerError> {
            self.calls.lock().unwrap().push(SinkCall::UploadBatch(
                SinkOwner::from(run),
                sequence,
                uploads.len(),
            ));
            Ok(())
        }

        fn terminalize(
            &self,
            run: &TraceRunOpen,
            outcome: TraceTerminalOutcome,
        ) -> Result<(), TraceProducerError> {
            self.calls
                .lock()
                .unwrap()
                .push(SinkCall::Terminal(SinkOwner::from(run), outcome));
            Ok(())
        }
    }

    impl TraceIdentityProvider for FixedIdentity {
        fn verified_identity(&self) -> Option<TraceIdentitySnapshot> {
            self.snapshot.lock().unwrap().clone()
        }

        fn with_verified_identity<R, F>(
            &self,
            expected: &TraceIdentitySnapshot,
            mutation: F,
        ) -> Option<R>
        where
            F: FnOnce() -> R,
        {
            let current = self.snapshot.lock().unwrap();
            (current.as_ref() == Some(expected)).then(mutation)
        }
    }

    impl TraceIdentityProvider for SwitchAfterSnapshotIdentity {
        fn verified_identity(&self) -> Option<TraceIdentitySnapshot> {
            let mut snapshot = self.snapshot.lock().unwrap();
            let captured = snapshot.clone();
            *snapshot = self.replacement.clone();
            captured
        }

        fn with_verified_identity<R, F>(
            &self,
            expected: &TraceIdentitySnapshot,
            mutation: F,
        ) -> Option<R>
        where
            F: FnOnce() -> R,
        {
            let current = self.snapshot.lock().unwrap();
            (current.as_ref() == Some(expected)).then(mutation)
        }
    }

    impl TraceAuthorizationProvider for FixedAuthorizer {
        fn authorize(
            &self,
            _operation: OperationKind,
            _run_id: TraceId,
            _identity: TraceIdentitySnapshot,
        ) -> BoxFuture<'static, Result<TraceAuthorization, TraceProducerError>> {
            let result = self.decision.clone();
            Box::pin(async move { result })
        }
    }

    impl TraceAuthorizationProvider for BlockingAuthorizer {
        fn authorize(
            &self,
            _operation: OperationKind,
            _run_id: TraceId,
            identity: TraceIdentitySnapshot,
        ) -> BoxFuture<'static, Result<TraceAuthorization, TraceProducerError>> {
            let started = self.started.clone();
            let release = self.release.clone();
            let observed = self.observed.clone();
            let decision = self.decision.clone();
            Box::pin(async move {
                observed.lock().unwrap().push(identity);
                started.notify_one();
                release.notified().await;
                decision
            })
        }
    }

    impl TraceMetadataSink for FailingSink {
        fn open_run(&self, run: &TraceRunOpen) -> Result<(), TraceProducerError> {
            self.calls
                .lock()
                .unwrap()
                .push(SinkCall::Open(SinkOwner::from(run), run.operation));
            self.open_runs.lock().unwrap().insert(run.run_id);
            Ok(())
        }

        fn record_authorization(
            &self,
            run: &TraceRunOpen,
            authorization: TraceAuthorization,
        ) -> Result<(), TraceProducerError> {
            self.calls
                .lock()
                .unwrap()
                .push(SinkCall::Authorization(SinkOwner::from(run), authorization));
            if self.fail_authorization {
                Err(TraceProducerError::Sink)
            } else {
                Ok(())
            }
        }

        fn record_authorization_terminal(
            &self,
            run: &TraceRunOpen,
            authorization: TraceAuthorization,
            outcome: TraceTerminalOutcome,
        ) -> Result<(), TraceProducerError> {
            let owner = SinkOwner::from(run);
            let mut calls = self.calls.lock().unwrap();
            calls.push(SinkCall::Authorization(owner.clone(), authorization));
            calls.push(SinkCall::Terminal(owner, outcome));
            self.open_runs.lock().unwrap().remove(&run.run_id);
            self.terminal_runs
                .lock()
                .unwrap()
                .insert(run.run_id, outcome);
            if self.fail_authorization || self.fail_terminal {
                Err(TraceProducerError::Sink)
            } else {
                Ok(())
            }
        }

        fn append_upload_attempts(
            &self,
            run: &TraceRunOpen,
            sequence: u64,
            uploads: &[SentinelAttestedTraceUpload],
        ) -> Result<(), TraceProducerError> {
            self.calls.lock().unwrap().push(SinkCall::UploadBatch(
                SinkOwner::from(run),
                sequence,
                uploads.len(),
            ));
            Ok(())
        }

        fn terminalize(
            &self,
            run: &TraceRunOpen,
            outcome: TraceTerminalOutcome,
        ) -> Result<(), TraceProducerError> {
            self.calls
                .lock()
                .unwrap()
                .push(SinkCall::Terminal(SinkOwner::from(run), outcome));
            self.open_runs.lock().unwrap().remove(&run.run_id);
            self.terminal_runs
                .lock()
                .unwrap()
                .insert(run.run_id, outcome);
            if self.fail_terminal {
                Err(TraceProducerError::Sink)
            } else {
                Ok(())
            }
        }
    }

    impl TraceMetadataSink for FailOnceBatchSink {
        fn open_run(&self, run: &TraceRunOpen) -> Result<(), TraceProducerError> {
            self.calls
                .lock()
                .unwrap()
                .push(SinkCall::Open(SinkOwner::from(run), run.operation));
            Ok(())
        }

        fn record_authorization(
            &self,
            run: &TraceRunOpen,
            authorization: TraceAuthorization,
        ) -> Result<(), TraceProducerError> {
            self.calls
                .lock()
                .unwrap()
                .push(SinkCall::Authorization(SinkOwner::from(run), authorization));
            Ok(())
        }

        fn record_authorization_terminal(
            &self,
            run: &TraceRunOpen,
            authorization: TraceAuthorization,
            outcome: TraceTerminalOutcome,
        ) -> Result<(), TraceProducerError> {
            let owner = SinkOwner::from(run);
            let mut calls = self.calls.lock().unwrap();
            calls.push(SinkCall::Authorization(owner.clone(), authorization));
            calls.push(SinkCall::Terminal(owner, outcome));
            Ok(())
        }

        fn append_upload_attempts(
            &self,
            run: &TraceRunOpen,
            sequence: u64,
            uploads: &[SentinelAttestedTraceUpload],
        ) -> Result<(), TraceProducerError> {
            self.attempted_upload_ids.lock().unwrap().push(
                uploads
                    .iter()
                    .map(SentinelAttestedTraceUpload::upload_id)
                    .collect(),
            );
            if self
                .fail_next
                .swap(false, std::sync::atomic::Ordering::AcqRel)
            {
                return Err(TraceProducerError::Sink);
            }
            self.calls.lock().unwrap().push(SinkCall::UploadBatch(
                SinkOwner::from(run),
                sequence,
                uploads.len(),
            ));
            Ok(())
        }

        fn terminalize(
            &self,
            run: &TraceRunOpen,
            outcome: TraceTerminalOutcome,
        ) -> Result<(), TraceProducerError> {
            self.calls
                .lock()
                .unwrap()
                .push(SinkCall::Terminal(SinkOwner::from(run), outcome));
            Ok(())
        }
    }

    fn identity(generation: u64) -> TraceIdentitySnapshot {
        TraceIdentitySnapshot::new([0x11; 32], generation, [0x22; 32])
    }

    fn producer(
        identity: Option<TraceIdentitySnapshot>,
        authorization: Result<TraceAuthorization, TraceProducerError>,
        sink: Arc<RecordingSink>,
    ) -> TraceProducer<FixedIdentity, FixedAuthorizer, Arc<RecordingSink>> {
        TraceProducer::new(
            FixedIdentity {
                snapshot: Arc::new(Mutex::new(identity)),
            },
            FixedAuthorizer {
                decision: authorization,
            },
            sink,
        )
    }

    fn event_text(event_id: TraceId, run_id: TraceId, sequence: u64) -> TraceEventText<'static> {
        TraceEventText {
            event_id,
            run_id,
            sequence,
            kind: TraceEventKindV2::Command,
            step_name: "bounded producer fixture",
            partition_name: None,
            status: TraceEventStatusV2::Success,
            started_at_ms: 1,
            ended_at_ms: Some(2),
            duration_ms: Some(1),
            command: None,
            exit_code: Some(0),
            verification: None,
            device_state: None,
            retry_safe: Some(true),
            remedies: &[],
            error_class: None,
            error_code: None,
            error_message: None,
        }
    }

    fn attest(upload: SealedTraceUpload) -> SentinelAttestedTraceUpload {
        SentinelAttestedTraceUpload::from_sealed_upload(upload).expect("sentinel-attested upload")
    }

    fn sealed_upload(run_id: TraceId, sequence: u64) -> SentinelAttestedTraceUpload {
        let event_id = TraceId::try_new_v7().expect("event UUIDv7");
        let mut reader = Cursor::new(b"safe trace output\n".to_vec());
        let upload = TraceOutputSession::from_reader(
            event_id,
            TraceOutputStreamV2::Stdout,
            &mut reader,
            &ExactSecretSet::empty(),
        )
        .expect("complete output stream")
        .into_event_upload_attempts(
            event_text(event_id, run_id, sequence),
            &ExactSecretSet::empty(),
        )
        .expect("bounded sealed upload")
        .into_iter()
        .next()
        .expect("one sealed upload");
        attest(upload)
    }

    fn sealed_upload_attempts_for(event_id: TraceId) -> Vec<SentinelAttestedTraceUpload> {
        let line = format!(
            "{}\n",
            "\t".repeat(nwflash_domain::TRACE_OUTPUT_MAX_BYTES - 1)
        );
        let mut reader = Cursor::new(line.repeat(40).into_bytes());
        let attempts = TraceOutputSession::from_reader(
            event_id,
            TraceOutputStreamV2::Stdout,
            &mut reader,
            &ExactSecretSet::empty(),
        )
        .expect("complete output stream")
        .into_upload_attempts()
        .expect("bounded sealed uploads");
        assert!(attempts.len() > 1);
        attempts.into_iter().map(attest).collect()
    }

    fn sealed_event_upload_attempts(
        run_id: TraceId,
        sequence: u64,
        event_id: TraceId,
    ) -> Vec<SentinelAttestedTraceUpload> {
        let line = format!(
            "{}\n",
            "\t".repeat(nwflash_domain::TRACE_OUTPUT_MAX_BYTES - 1)
        );
        let mut reader = Cursor::new(line.repeat(40).into_bytes());
        let attempts = TraceOutputSession::from_reader(
            event_id,
            TraceOutputStreamV2::Stdout,
            &mut reader,
            &ExactSecretSet::empty(),
        )
        .expect("complete output stream")
        .into_event_upload_attempts(
            event_text(event_id, run_id, sequence),
            &ExactSecretSet::empty(),
        )
        .expect("bounded sealed event uploads");
        attempts.into_iter().map(attest).collect()
    }

    fn manifest_only_upload(
        upload_id: TraceId,
        event_id: TraceId,
        run_id: TraceId,
        sequence: u64,
    ) -> SentinelAttestedTraceUpload {
        let event = RedactedTraceEvent::try_new(
            event_text(event_id, run_id, sequence),
            &ExactSecretSet::empty(),
            None,
            None,
        )
        .expect("redacted event manifest");
        attest(
            SealedTraceUpload::new(upload_id, Vec::new(), vec![event])
                .expect("bounded manifest-only upload"),
        )
    }

    fn run_only_upload(upload_id: TraceId, run_id: TraceId) -> SentinelAttestedTraceUpload {
        let run = RedactedTraceRun::try_new(
            TraceRunText {
                run_id,
                operation_kind: "flashing",
                title: "foreign run metadata",
                outcome: TraceOutcomeV2::Running,
                device_serial: None,
                source_paths: &[],
                source_urls: &[],
                client_version: "test",
                started_at_ms: 1,
                ended_at_ms: None,
                duration_ms: None,
                error_class: None,
                error_code: None,
                error_message: None,
                final_sequence: None,
                trace_complete: false,
                trace_loss_reason: None,
            },
            &ExactSecretSet::empty(),
        )
        .expect("redacted run metadata");
        attest(
            SealedTraceUpload::new(upload_id, vec![run], Vec::new())
                .expect("bounded run-only upload"),
        )
    }

    #[tokio::test]
    async fn unauthenticated_start_does_not_allocate_id_or_touch_sink() {
        let sink = Arc::new(RecordingSink::default());
        let result = producer(None, Ok(TraceAuthorization::Allowed), sink.clone())
            .start(OperationKind::Flashing)
            .await;

        assert!(matches!(result, Err(TraceProducerError::Unauthenticated)));
        assert!(sink.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn identity_switch_after_capture_prevents_the_open_sink_mutation() {
        let sink = Arc::new(RecordingSink::default());
        let result = TraceProducer::new(
            SwitchAfterSnapshotIdentity {
                snapshot: Mutex::new(Some(identity(1))),
                replacement: Some(TraceIdentitySnapshot::new([0x33; 32], 2, [0x44; 32])),
            },
            FixedAuthorizer {
                decision: Ok(TraceAuthorization::Allowed),
            },
            sink.clone(),
        )
        .start(OperationKind::Flashing)
        .await;

        assert!(matches!(result, Err(TraceProducerError::StaleIdentity)));
        assert!(sink.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn lifecycle_states_are_not_valid_trace_start_kinds() {
        for operation in [
            OperationKind::Idle,
            OperationKind::Completed,
            OperationKind::Canceled,
            OperationKind::Failed,
        ] {
            let sink = Arc::new(RecordingSink::default());
            let result = producer(
                Some(identity(1)),
                Ok(TraceAuthorization::Allowed),
                sink.clone(),
            )
            .start(operation)
            .await;
            assert!(matches!(
                result,
                Err(TraceProducerError::InvalidOperationKind)
            ));
            assert!(sink.calls.lock().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn all_seven_operation_kinds_open_trace_runs() {
        let operations = [
            OperationKind::Discovering,
            OperationKind::Rebooting,
            OperationKind::Installing,
            OperationKind::Transferring,
            OperationKind::Hashing,
            OperationKind::Flashing,
            OperationKind::Mirroring,
        ];
        for operation in operations {
            let sink = Arc::new(RecordingSink::default());
            assert!(
                producer(Some(identity(1)), Ok(TraceAuthorization::Allowed), sink)
                    .start(operation)
                    .await
                    .is_ok(),
                "{operation:?} must be a trace start kind"
            );
        }
    }

    #[tokio::test]
    async fn open_run_is_persisted_before_remote_authorization_and_id_is_uuid_v7() {
        let sink = Arc::new(RecordingSink::default());
        let run = producer(
            Some(identity(7)),
            Ok(TraceAuthorization::Allowed),
            sink.clone(),
        )
        .start(OperationKind::Flashing)
        .await
        .unwrap();

        assert_eq!(run.run_id().as_uuid().get_version_num(), 7);
        assert_eq!(
            sink.calls.lock().unwrap().as_slice(),
            &[
                SinkCall::Open(
                    SinkOwner {
                        run_id: run.run_id(),
                        identity_epoch: 7,
                        owner_username_hash: [0x11; 32],
                        build_hash: [0x22; 32],
                    },
                    OperationKind::Flashing,
                ),
                SinkCall::Authorization(
                    SinkOwner {
                        run_id: run.run_id(),
                        identity_epoch: 7,
                        owner_username_hash: [0x11; 32],
                        build_hash: [0x22; 32],
                    },
                    TraceAuthorization::Allowed,
                ),
            ]
        );
    }

    #[tokio::test]
    async fn denied_authorization_is_recorded_and_terminalized() {
        let sink = Arc::new(RecordingSink::default());
        let run = producer(
            Some(identity(1)),
            Ok(TraceAuthorization::Denied),
            sink.clone(),
        )
        .start(OperationKind::Installing)
        .await;

        let calls = sink.calls.lock().unwrap();
        assert!(matches!(run, Err(TraceProducerError::Denied)));
        assert!(matches!(
            calls[2],
            SinkCall::Terminal(_, TraceTerminalOutcome::Denied)
        ));
    }

    #[tokio::test]
    async fn advisory_allow_is_accepted_for_safe_verification() {
        let sink = Arc::new(RecordingSink::default());
        let run = producer(
            Some(identity(1)),
            Ok(TraceAuthorization::AdvisoryAllowed),
            sink.clone(),
        )
        .start(OperationKind::Hashing)
        .await
        .unwrap();

        assert_eq!(run.authorization(), TraceAuthorization::AdvisoryAllowed);
    }

    #[tokio::test]
    async fn stale_identity_rejects_append_and_finalize() {
        let sink = Arc::new(RecordingSink::default());
        let identity_provider = Arc::new(FixedIdentity {
            snapshot: Arc::new(Mutex::new(Some(identity(4)))),
        });
        let producer = TraceProducer::new(
            identity_provider.clone(),
            FixedAuthorizer {
                decision: Ok(TraceAuthorization::Allowed),
            },
            sink.clone(),
        );
        let run = producer.start(OperationKind::Discovering).await.unwrap();
        run.reserve_sequence(1).unwrap();
        *identity_provider.snapshot.lock().unwrap() = Some(identity(5));
        assert!(matches!(
            run.append_upload(1, &sealed_upload(run.run_id(), 1)),
            Err(TraceProducerError::StaleIdentity)
        ));
        assert!(!sink
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| matches!(call, SinkCall::UploadBatch(_, 1, _))));
        assert!(matches!(
            run.finalize(TraceTerminalOutcome::Success),
            Err(TraceProducerError::StaleIdentity)
        ));
    }

    #[tokio::test]
    async fn terminal_slot_is_reserved_and_sequences_are_contiguous() {
        let sink = Arc::new(RecordingSink::default());
        let run = producer(Some(identity(1)), Ok(TraceAuthorization::Allowed), sink)
            .start(OperationKind::Discovering)
            .await
            .unwrap();
        assert_eq!(run.next_sequence(), 1);
        let seq = run.reserve_sequence(1).unwrap();
        assert_eq!(seq, 1);
        assert!(matches!(
            run.reserve_sequence(1),
            Err(TraceProducerError::SequenceGap)
        ));
        assert!(matches!(
            run.reserve_sequence(3),
            Err(TraceProducerError::SequenceGap)
        ));
        assert_eq!(run.next_sequence(), 2);
    }

    #[tokio::test]
    async fn reserved_sequence_must_be_consumed_by_a_sealed_upload_before_finalize() {
        let sink = Arc::new(RecordingSink::default());
        let run = producer(
            Some(identity(1)),
            Ok(TraceAuthorization::Allowed),
            sink.clone(),
        )
        .start(OperationKind::Discovering)
        .await
        .unwrap();

        assert_eq!(run.reserve_sequence(1).unwrap(), 1);
        assert!(matches!(
            run.finalize(TraceTerminalOutcome::Success),
            Err(TraceProducerError::SequenceGap)
        ));

        run.append_upload(1, &sealed_upload(run.run_id(), 1))
            .unwrap();
        assert!(matches!(
            run.append_upload(1, &sealed_upload(run.run_id(), 1)),
            Err(TraceProducerError::SequenceGap)
        ));
        assert!(matches!(
            run.reserve_sequence(3),
            Err(TraceProducerError::SequenceGap)
        ));

        run.finalize(TraceTerminalOutcome::Success).unwrap();
        assert!(run.is_terminal());
        assert!(matches!(
            sink.calls.lock().unwrap().last(),
            Some(SinkCall::Terminal(_, TraceTerminalOutcome::Success))
        ));
    }

    #[tokio::test]
    async fn reserved_sequence_blocks_later_unreserved_append() {
        let sink = Arc::new(RecordingSink::default());
        let run = producer(
            Some(identity(1)),
            Ok(TraceAuthorization::Allowed),
            sink.clone(),
        )
        .start(OperationKind::Discovering)
        .await
        .unwrap();

        run.reserve_sequence(1).unwrap();
        assert!(matches!(
            run.append_upload(2, &sealed_upload(run.run_id(), 2)),
            Err(TraceProducerError::SequenceGap)
        ));
        assert!(!sink
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| matches!(call, SinkCall::UploadBatch(_, 2, _))));

        run.append_upload(1, &sealed_upload(run.run_id(), 1))
            .unwrap();
        run.finalize(TraceTerminalOutcome::Success).unwrap();
    }

    #[tokio::test]
    async fn multiple_upload_attempts_consume_one_event_sequence_atomically() {
        let sink = Arc::new(RecordingSink::default());
        let run = producer(
            Some(identity(1)),
            Ok(TraceAuthorization::Allowed),
            sink.clone(),
        )
        .start(OperationKind::Transferring)
        .await
        .unwrap();

        run.reserve_sequence(1).unwrap();
        let attempts = sealed_event_upload_attempts(
            run.run_id(),
            1,
            TraceId::try_new_v7().expect("first event UUIDv7"),
        );
        let attempt_count = attempts.len();
        run.append_upload_attempts(1, &attempts).unwrap();
        assert_eq!(run.next_sequence(), 2);
        assert_eq!(run.reserve_sequence(2).unwrap(), 2);
        run.append_upload(2, &sealed_upload(run.run_id(), 2))
            .unwrap();
        assert_eq!(run.next_sequence(), 3);
        assert!(matches!(
            run.append_upload_attempts(
                1,
                &sealed_event_upload_attempts(
                    run.run_id(),
                    1,
                    TraceId::try_new_v7().expect("repeated event UUIDv7"),
                ),
            ),
            Err(TraceProducerError::SequenceGap)
        ));
        assert!(sink.calls.lock().unwrap().iter().any(|call| matches!(
            call,
            SinkCall::UploadBatch(_, 1, count) if *count == attempt_count
        )));
        assert!(sink
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| matches!(call, SinkCall::UploadBatch(_, 2, 1))));
        run.finalize(TraceTerminalOutcome::Success).unwrap();
    }

    #[tokio::test]
    async fn mixed_event_attempts_fail_closed_without_consuming_the_reservation() {
        let sink = Arc::new(RecordingSink::default());
        let run = producer(
            Some(identity(1)),
            Ok(TraceAuthorization::Allowed),
            sink.clone(),
        )
        .start(OperationKind::Transferring)
        .await
        .unwrap();
        let mut attempts = sealed_event_upload_attempts(
            run.run_id(),
            1,
            TraceId::try_new_v7().expect("first event UUIDv7"),
        );
        attempts.extend(sealed_event_upload_attempts(
            run.run_id(),
            1,
            TraceId::try_new_v7().expect("second event UUIDv7"),
        ));

        run.reserve_sequence(1).unwrap();
        assert!(matches!(
            run.append_upload_attempts(1, &attempts),
            Err(TraceProducerError::MixedEventAttempts)
        ));
        assert!(!sink
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| matches!(call, SinkCall::UploadBatch(_, 1, _))));
        assert!(matches!(
            run.finalize(TraceTerminalOutcome::Success),
            Err(TraceProducerError::SequenceGap)
        ));
    }

    #[tokio::test]
    async fn foreign_run_or_sequence_event_manifest_is_rejected_before_sink_mutation() {
        let sink = Arc::new(RecordingSink::default());
        let run = producer(
            Some(identity(1)),
            Ok(TraceAuthorization::Allowed),
            sink.clone(),
        )
        .start(OperationKind::Transferring)
        .await
        .unwrap();
        let foreign_attempts = sealed_event_upload_attempts(
            TraceId::try_new_v7().expect("foreign run UUIDv7"),
            1,
            TraceId::try_new_v7().expect("foreign event UUIDv7"),
        );

        run.reserve_sequence(1).unwrap();
        assert!(matches!(
            run.append_upload_attempts(1, &foreign_attempts),
            Err(TraceProducerError::EventBindingMismatch)
        ));
        let wrong_sequence_attempts = sealed_event_upload_attempts(
            run.run_id(),
            2,
            TraceId::try_new_v7().expect("wrong-sequence event UUIDv7"),
        );
        assert!(matches!(
            run.append_upload_attempts(1, &wrong_sequence_attempts),
            Err(TraceProducerError::EventBindingMismatch)
        ));
        let output_only_attempts =
            sealed_upload_attempts_for(TraceId::try_new_v7().expect("unbound event UUIDv7"));
        assert!(matches!(
            run.append_upload_attempts(1, &output_only_attempts),
            Err(TraceProducerError::UnboundEventAttempts)
        ));
        assert!(!sink
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| matches!(call, SinkCall::UploadBatch(_, 1, _))));
        assert!(matches!(
            run.finalize(TraceTerminalOutcome::Success),
            Err(TraceProducerError::SequenceGap)
        ));
    }

    #[tokio::test]
    async fn duplicate_reordered_and_unbounded_attempt_batches_fail_closed() {
        let sink = Arc::new(RecordingSink::default());
        let run = producer(
            Some(identity(1)),
            Ok(TraceAuthorization::Allowed),
            sink.clone(),
        )
        .start(OperationKind::Transferring)
        .await
        .unwrap();
        run.reserve_sequence(1).unwrap();
        let event_id = TraceId::try_new_v7().expect("event UUIDv7");
        let duplicate_upload_id = TraceId::try_new_v7().expect("duplicate upload UUIDv7");
        let duplicates = vec![
            manifest_only_upload(duplicate_upload_id, event_id, run.run_id(), 1),
            manifest_only_upload(duplicate_upload_id, event_id, run.run_id(), 1),
        ];
        assert!(matches!(
            run.append_upload_attempts(1, &duplicates),
            Err(TraceProducerError::DuplicateUploadAttempt)
        ));

        let mut reordered = sealed_event_upload_attempts(run.run_id(), 1, event_id);
        assert!(reordered.len() > 2);
        reordered.swap(1, 2);
        assert!(matches!(
            run.append_upload_attempts(1, &reordered),
            Err(TraceProducerError::UploadAttemptOrder)
        ));

        let unbounded = (0..=MAX_TRACE_EVENT_UPLOAD_ATTEMPTS)
            .map(|_| {
                manifest_only_upload(
                    TraceId::try_new_v7().expect("upload UUIDv7"),
                    event_id,
                    run.run_id(),
                    1,
                )
            })
            .collect::<Vec<_>>();
        assert!(matches!(
            run.append_upload_attempts(1, &unbounded),
            Err(TraceProducerError::UploadBatchLimit)
        ));
        assert!(!sink
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| matches!(call, SinkCall::UploadBatch(_, 1, _))));
        assert!(matches!(
            run.finalize(TraceTerminalOutcome::Success),
            Err(TraceProducerError::SequenceGap)
        ));
    }

    #[tokio::test]
    async fn event_batch_rejects_run_records_and_a_late_event_manifest() {
        let sink = Arc::new(RecordingSink::default());
        let run = producer(
            Some(identity(1)),
            Ok(TraceAuthorization::Allowed),
            sink.clone(),
        )
        .start(OperationKind::Transferring)
        .await
        .unwrap();
        let event_id = TraceId::try_new_v7().expect("event UUIDv7");
        let mut with_run = sealed_event_upload_attempts(run.run_id(), 1, event_id);
        with_run.push(run_only_upload(
            TraceId::try_new_v7().expect("run upload UUIDv7"),
            TraceId::try_new_v7().expect("foreign run UUIDv7"),
        ));

        run.reserve_sequence(1).unwrap();
        assert!(matches!(
            run.append_upload_attempts(1, &with_run),
            Err(TraceProducerError::UnexpectedRunMetadata)
        ));

        let mut late_manifest = sealed_upload_attempts_for(event_id);
        late_manifest.push(manifest_only_upload(
            TraceId::try_new_v7().expect("manifest upload UUIDv7"),
            event_id,
            run.run_id(),
            1,
        ));
        assert!(matches!(
            run.append_upload_attempts(1, &late_manifest),
            Err(TraceProducerError::EventManifestOrder)
        ));
        assert!(!sink
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| matches!(call, SinkCall::UploadBatch(_, 1, _))));
        assert!(matches!(
            run.finalize(TraceTerminalOutcome::Success),
            Err(TraceProducerError::SequenceGap)
        ));
    }

    #[tokio::test]
    async fn failed_upload_batch_keeps_the_reservation_for_exact_retry() {
        let sink = Arc::new(FailOnceBatchSink {
            calls: Mutex::new(Vec::new()),
            attempted_upload_ids: Mutex::new(Vec::new()),
            fail_next: std::sync::atomic::AtomicBool::new(true),
        });
        let run = TraceProducer::new(
            FixedIdentity {
                snapshot: Arc::new(Mutex::new(Some(identity(1)))),
            },
            FixedAuthorizer {
                decision: Ok(TraceAuthorization::Allowed),
            },
            sink.clone(),
        )
        .start(OperationKind::Transferring)
        .await
        .unwrap();

        run.reserve_sequence(1).unwrap();
        let attempts = sealed_event_upload_attempts(
            run.run_id(),
            1,
            TraceId::try_new_v7().expect("event UUIDv7"),
        );
        let upload_ids = attempts
            .iter()
            .map(SentinelAttestedTraceUpload::upload_id)
            .collect::<Vec<_>>();
        assert!(matches!(
            run.append_upload_attempts(1, &attempts),
            Err(TraceProducerError::Sink)
        ));
        assert!(!sink
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| matches!(call, SinkCall::UploadBatch(_, 1, _))));
        assert!(matches!(
            run.finalize(TraceTerminalOutcome::Success),
            Err(TraceProducerError::SequenceGap)
        ));

        run.append_upload_attempts(1, &attempts).unwrap();
        assert_eq!(
            attempts
                .iter()
                .map(SentinelAttestedTraceUpload::upload_id)
                .collect::<Vec<_>>(),
            upload_ids
        );
        assert_eq!(
            sink.attempted_upload_ids.lock().unwrap().as_slice(),
            &[upload_ids.clone(), upload_ids.clone()]
        );
        assert!(sink.calls.lock().unwrap().iter().any(|call| matches!(
            call, SinkCall::UploadBatch(_, 1, count) if *count == upload_ids.len()
        )));
        run.finalize(TraceTerminalOutcome::Success).unwrap();
    }

    #[tokio::test]
    async fn truncated_attempt_tail_cannot_consume_the_event_reservation() {
        let sink = Arc::new(RecordingSink::default());
        let run = producer(
            Some(identity(1)),
            Ok(TraceAuthorization::Allowed),
            sink.clone(),
        )
        .start(OperationKind::Transferring)
        .await
        .unwrap();
        let mut attempts = sealed_event_upload_attempts(
            run.run_id(),
            1,
            TraceId::try_new_v7().expect("event UUIDv7"),
        );
        assert!(attempts.len() > 1);
        attempts.pop();

        run.reserve_sequence(1).unwrap();
        assert!(matches!(
            run.append_upload_attempts(1, &attempts),
            Err(TraceProducerError::IncompleteEventAttempts)
        ));
        assert!(!sink
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| matches!(call, SinkCall::UploadBatch(_, 1, _))));
        assert!(matches!(
            run.finalize(TraceTerminalOutcome::Success),
            Err(TraceProducerError::SequenceGap)
        ));
    }

    #[tokio::test]
    async fn failed_single_upload_can_retry_the_same_upload_id() {
        let sink = Arc::new(FailOnceBatchSink {
            calls: Mutex::new(Vec::new()),
            attempted_upload_ids: Mutex::new(Vec::new()),
            fail_next: std::sync::atomic::AtomicBool::new(true),
        });
        let run = TraceProducer::new(
            FixedIdentity {
                snapshot: Arc::new(Mutex::new(Some(identity(1)))),
            },
            FixedAuthorizer {
                decision: Ok(TraceAuthorization::Allowed),
            },
            sink.clone(),
        )
        .start(OperationKind::Transferring)
        .await
        .unwrap();
        let upload = sealed_upload(run.run_id(), 1);
        let upload_id = upload.upload_id();

        run.reserve_sequence(1).unwrap();
        assert!(matches!(
            run.append_upload(1, &upload),
            Err(TraceProducerError::Sink)
        ));
        assert_eq!(upload.upload_id(), upload_id);
        run.append_upload(1, &upload).unwrap();
        assert_eq!(upload.upload_id(), upload_id);
        assert_eq!(
            sink.attempted_upload_ids.lock().unwrap().as_slice(),
            &[vec![upload_id], vec![upload_id]]
        );
        run.finalize(TraceTerminalOutcome::Success).unwrap();
    }

    #[tokio::test]
    async fn sequence_hundred_is_consumable_and_only_101_hits_the_limit() {
        let sink = Arc::new(RecordingSink::default());
        let run = producer(Some(identity(1)), Ok(TraceAuthorization::Allowed), sink)
            .start(OperationKind::Discovering)
            .await
            .unwrap();

        for sequence in 1..=100 {
            assert_eq!(run.reserve_sequence(sequence).unwrap(), sequence);
            run.append_upload(sequence, &sealed_upload(run.run_id(), sequence))
                .unwrap();
        }
        assert!(matches!(
            run.reserve_sequence(101),
            Err(TraceProducerError::SequenceLimit)
        ));
        run.finalize(TraceTerminalOutcome::Success).unwrap();
    }

    #[tokio::test]
    async fn unreserved_append_also_accepts_sequence_hundred_but_rejects_101() {
        let sink = Arc::new(RecordingSink::default());
        let run = producer(Some(identity(1)), Ok(TraceAuthorization::Allowed), sink)
            .start(OperationKind::Discovering)
            .await
            .unwrap();

        for sequence in 1..=100 {
            run.append_upload(sequence, &sealed_upload(run.run_id(), sequence))
                .unwrap();
        }
        assert_eq!(run.next_sequence(), 101);
        assert!(matches!(
            run.append_upload(101, &sealed_upload(run.run_id(), 100)),
            Err(TraceProducerError::SequenceLimit)
        ));
        run.finalize(TraceTerminalOutcome::Success).unwrap();
    }

    #[tokio::test]
    async fn identity_change_while_authorizing_aborts_the_open_owner_run() {
        let identity_provider = Arc::new(FixedIdentity {
            snapshot: Arc::new(Mutex::new(Some(identity(1)))),
        });
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let observed = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::new(RecordingSink::default());
        let producer = TraceProducer::new(
            identity_provider.clone(),
            BlockingAuthorizer {
                started: started.clone(),
                release: release.clone(),
                observed: observed.clone(),
                decision: Ok(TraceAuthorization::Allowed),
            },
            sink.clone(),
        );

        let task = tokio::spawn(async move { producer.start(OperationKind::Flashing).await });
        started.notified().await;
        *identity_provider.snapshot.lock().unwrap() =
            Some(TraceIdentitySnapshot::new([0x33; 32], 2, [0x44; 32]));
        release.notify_one();

        assert!(matches!(
            task.await.unwrap(),
            Err(TraceProducerError::StaleIdentity)
        ));
        let calls = sink.calls.lock().unwrap();
        assert_eq!(observed.lock().unwrap().as_slice(), &[identity(1)]);
        let SinkCall::Open(owner, OperationKind::Flashing) = &calls[0] else {
            panic!("first sink call must open owner A's run");
        };
        assert_eq!(owner.identity_epoch, 1);
        assert_eq!(owner.owner_username_hash, [0x11; 32]);
        assert_eq!(owner.build_hash, [0x22; 32]);
        assert_eq!(calls.len(), 2);
        let SinkCall::Terminal(terminal_owner, TraceTerminalOutcome::Aborted) = &calls[1] else {
            panic!("stale authorization must abort owner A's run");
        };
        assert_eq!(terminal_owner, owner);
    }

    #[tokio::test]
    async fn authorizer_error_after_identity_switch_aborts_a_instead_of_denying_it() {
        let identity_provider = Arc::new(FixedIdentity {
            snapshot: Arc::new(Mutex::new(Some(identity(1)))),
        });
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let sink = Arc::new(RecordingSink::default());
        let producer = TraceProducer::new(
            identity_provider.clone(),
            BlockingAuthorizer {
                started: started.clone(),
                release: release.clone(),
                observed: Arc::new(Mutex::new(Vec::new())),
                decision: Err(TraceProducerError::AuthorizationFailed),
            },
            sink.clone(),
        );

        let task = tokio::spawn(async move { producer.start(OperationKind::Flashing).await });
        started.notified().await;
        *identity_provider.snapshot.lock().unwrap() =
            Some(TraceIdentitySnapshot::new([0x33; 32], 2, [0x44; 32]));
        release.notify_one();

        assert!(matches!(
            task.await.unwrap(),
            Err(TraceProducerError::StaleIdentity)
        ));
        let calls = sink.calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert!(matches!(
            calls[0],
            SinkCall::Open(_, OperationKind::Flashing)
        ));
        assert!(matches!(
            calls[1],
            SinkCall::Terminal(_, TraceTerminalOutcome::Aborted)
        ));
        assert!(!calls
            .iter()
            .any(|call| matches!(call, SinkCall::Authorization(_, _))));
    }

    #[tokio::test]
    async fn stale_identity_terminal_diagnostic_error_still_leaves_owner_a_aborted() {
        let identity_provider = Arc::new(FixedIdentity {
            snapshot: Arc::new(Mutex::new(Some(identity(1)))),
        });
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let sink = Arc::new(FailingSink {
            calls: Mutex::new(Vec::new()),
            open_runs: Mutex::new(BTreeSet::new()),
            terminal_runs: Mutex::new(std::collections::BTreeMap::new()),
            fail_authorization: false,
            fail_terminal: true,
        });
        let producer = TraceProducer::new(
            identity_provider.clone(),
            BlockingAuthorizer {
                started: started.clone(),
                release: release.clone(),
                observed: Arc::new(Mutex::new(Vec::new())),
                decision: Err(TraceProducerError::AuthorizationFailed),
            },
            sink.clone(),
        );

        let task = tokio::spawn(async move { producer.start(OperationKind::Flashing).await });
        started.notified().await;
        *identity_provider.snapshot.lock().unwrap() =
            Some(TraceIdentitySnapshot::new([0x33; 32], 2, [0x44; 32]));
        release.notify_one();

        assert!(matches!(task.await.unwrap(), Err(TraceProducerError::Sink)));
        assert!(sink.open_runs.lock().unwrap().is_empty());
        let terminal_runs = sink.terminal_runs.lock().unwrap();
        assert_eq!(terminal_runs.len(), 1);
        assert!(terminal_runs
            .values()
            .all(|outcome| *outcome == TraceTerminalOutcome::Aborted));
    }

    #[tokio::test]
    async fn allowed_authorization_record_error_still_leaves_the_open_run_aborted() {
        let sink = Arc::new(FailingSink {
            calls: Mutex::new(Vec::new()),
            open_runs: Mutex::new(BTreeSet::new()),
            terminal_runs: Mutex::new(std::collections::BTreeMap::new()),
            fail_authorization: true,
            fail_terminal: true,
        });
        let result = TraceProducer::new(
            FixedIdentity {
                snapshot: Arc::new(Mutex::new(Some(identity(1)))),
            },
            FixedAuthorizer {
                decision: Ok(TraceAuthorization::Allowed),
            },
            sink.clone(),
        )
        .start(OperationKind::Flashing)
        .await;

        assert!(matches!(result, Err(TraceProducerError::Sink)));
        assert!(sink.open_runs.lock().unwrap().is_empty());
        let terminal_runs = sink.terminal_runs.lock().unwrap();
        assert_eq!(terminal_runs.len(), 1);
        assert!(terminal_runs
            .values()
            .all(|outcome| *outcome == TraceTerminalOutcome::Aborted));
    }

    #[tokio::test]
    async fn authorizer_error_prioritizes_sink_failure_and_attempts_terminalization() {
        for (fail_authorization, fail_terminal) in
            [(false, false), (true, false), (false, true), (true, true)]
        {
            let sink = Arc::new(FailingSink {
                calls: Mutex::new(Vec::new()),
                open_runs: Mutex::new(BTreeSet::new()),
                terminal_runs: Mutex::new(std::collections::BTreeMap::new()),
                fail_authorization,
                fail_terminal,
            });
            let result = TraceProducer::new(
                FixedIdentity {
                    snapshot: Arc::new(Mutex::new(Some(identity(1)))),
                },
                FixedAuthorizer {
                    decision: Err(TraceProducerError::AuthorizationFailed),
                },
                sink.clone(),
            )
            .start(OperationKind::Flashing)
            .await;

            if fail_authorization || fail_terminal {
                assert!(matches!(result, Err(TraceProducerError::Sink)));
            } else {
                assert!(matches!(
                    result,
                    Err(TraceProducerError::AuthorizationFailed)
                ));
            }
            let calls = sink.calls.lock().unwrap();
            assert!(calls.iter().any(|call| matches!(
                call,
                SinkCall::Authorization(_, TraceAuthorization::Denied)
            )));
            assert!(calls
                .iter()
                .any(|call| matches!(call, SinkCall::Terminal(_, TraceTerminalOutcome::Denied))));
            assert!(sink.open_runs.lock().unwrap().is_empty());
            let terminal_runs = sink.terminal_runs.lock().unwrap();
            assert_eq!(terminal_runs.len(), 1);
            assert!(terminal_runs
                .values()
                .all(|outcome| *outcome == TraceTerminalOutcome::Denied));
        }
    }

    #[tokio::test]
    async fn parallel_event_calls_are_serialized() {
        let sink = Arc::new(RecordingSink::default());
        let run = producer(
            Some(identity(1)),
            Ok(TraceAuthorization::Allowed),
            sink.clone(),
        )
        .start(OperationKind::Transferring)
        .await
        .unwrap();
        let first = run.reserve_sequence(1).unwrap();
        let second = run.reserve_sequence(2).unwrap();
        assert_eq!((first, second), (1, 2));
    }

    #[tokio::test]
    async fn every_terminal_outcome_marks_run_non_idle() {
        let sink = Arc::new(RecordingSink::default());
        let run = producer(
            Some(identity(1)),
            Ok(TraceAuthorization::Allowed),
            sink.clone(),
        )
        .start(OperationKind::Mirroring)
        .await
        .unwrap();
        run.finalize(TraceTerminalOutcome::Failed).unwrap();
        assert!(run.is_terminal());
        assert!(matches!(
            run.finalize(TraceTerminalOutcome::Success),
            Err(TraceProducerError::AlreadyTerminal)
        ));
    }
}
