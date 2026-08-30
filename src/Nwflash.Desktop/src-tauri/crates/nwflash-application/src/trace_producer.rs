use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use futures::future::BoxFuture;

use nwflash_domain::{OperationKind, TraceId};
use nwflash_protection::SealedTraceUpload;

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

pub trait TraceIdentityProvider: Send + Sync {
    fn verified_identity(&self) -> Option<TraceIdentitySnapshot>;
}

impl<T> TraceIdentityProvider for Arc<T>
where
    T: TraceIdentityProvider + ?Sized,
{
    fn verified_identity(&self) -> Option<TraceIdentitySnapshot> {
        (**self).verified_identity()
    }
}

pub trait TraceAuthorizationProvider: Send + Sync {
    fn authorize(
        &self,
        operation: OperationKind,
        run_id: TraceId,
    ) -> BoxFuture<'static, Result<TraceAuthorization, TraceProducerError>>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceTerminalOutcome {
    Success,
    Failed,
    Canceled,
    Denied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TraceRunOpen {
    pub run_id: TraceId,
    pub operation: OperationKind,
    pub identity_epoch: u64,
}

pub trait TraceMetadataSink: Send + Sync {
    fn open_run(&self, run: &TraceRunOpen) -> Result<(), TraceProducerError>;
    fn record_authorization(
        &self,
        run_id: TraceId,
        authorization: TraceAuthorization,
    ) -> Result<(), TraceProducerError>;
    fn append_upload(
        &self,
        run_id: TraceId,
        sequence: u64,
        upload: SealedTraceUpload,
    ) -> Result<(), TraceProducerError>;
    fn terminalize(
        &self,
        run_id: TraceId,
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
        run_id: TraceId,
        authorization: TraceAuthorization,
    ) -> Result<(), TraceProducerError> {
        (**self).record_authorization(run_id, authorization)
    }

    fn append_upload(
        &self,
        run_id: TraceId,
        sequence: u64,
        upload: SealedTraceUpload,
    ) -> Result<(), TraceProducerError> {
        (**self).append_upload(run_id, sequence, upload)
    }

    fn terminalize(
        &self,
        run_id: TraceId,
        outcome: TraceTerminalOutcome,
    ) -> Result<(), TraceProducerError> {
        (**self).terminalize(run_id, outcome)
    }
}

#[derive(Clone)]
pub struct TraceProducer<I, A, S> {
    identity: Arc<I>,
    authorization: Arc<A>,
    sink: Arc<S>,
}

pub struct TraceRunHandle<I, S> {
    run_id: TraceId,
    operation: OperationKind,
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
        self.sink
            .open_run(&TraceRunOpen {
                run_id,
                operation,
                identity_epoch: identity.epoch(),
            })
            .map_err(|_| TraceProducerError::Sink)?;

        let authorization = match self.authorization.authorize(operation, run_id).await {
            Ok(value) => value,
            Err(_) => {
                let _ = self
                    .sink
                    .record_authorization(run_id, TraceAuthorization::Denied);
                let _ = self.sink.terminalize(run_id, TraceTerminalOutcome::Denied);
                return Err(TraceProducerError::AuthorizationFailed);
            }
        };
        self.sink
            .record_authorization(run_id, authorization)
            .map_err(|_| TraceProducerError::Sink)?;
        let handle = TraceRunHandle {
            run_id,
            operation,
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
        if authorization == TraceAuthorization::Denied {
            self.sink
                .terminalize(run_id, TraceTerminalOutcome::Denied)
                .map_err(|_| TraceProducerError::Sink)?;
            return Err(TraceProducerError::Denied);
        }
        Ok(handle)
    }
}

impl<I, S> TraceRunHandle<I, S>
where
    I: TraceIdentityProvider + 'static,
    S: TraceMetadataSink + 'static,
{
    pub const fn run_id(&self) -> TraceId {
        self.run_id
    }

    pub const fn authorization(&self) -> TraceAuthorization {
        self.authorization
    }

    pub const fn operation(&self) -> OperationKind {
        self.operation
    }

    pub fn is_terminal(&self) -> bool {
        self.state.lock().expect("trace state lock").terminal
    }

    pub fn next_sequence(&self) -> u64 {
        self.state.lock().expect("trace state lock").next_sequence
    }

    pub fn reserve_sequence(&self, sequence: u64) -> Result<u64, TraceProducerError> {
        self.ensure_current()?;
        let mut state = self.state.lock().expect("trace state lock");
        if state.terminal {
            return Err(TraceProducerError::AlreadyTerminal);
        }
        if sequence != state.next_sequence {
            return Err(TraceProducerError::SequenceGap);
        }
        if sequence >= nwflash_domain::TRACE_RUN_MAX_EVENTS as u64 {
            return Err(TraceProducerError::SequenceLimit);
        }
        state.next_sequence = state.next_sequence.saturating_add(1);
        state.reserved_sequences.insert(sequence);
        Ok(sequence)
    }

    pub fn append_upload(
        &self,
        sequence: u64,
        upload: SealedTraceUpload,
    ) -> Result<(), TraceProducerError> {
        self.ensure_current()?;
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
            if sequence >= nwflash_domain::TRACE_RUN_MAX_EVENTS as u64 {
                return Err(TraceProducerError::SequenceLimit);
            }
        }
        self.sink
            .append_upload(self.run_id, sequence, upload)
            .map_err(|_| TraceProducerError::Sink)?;
        if reserved {
            state.reserved_sequences.remove(&sequence);
        } else {
            state.next_sequence = state.next_sequence.saturating_add(1);
        }
        Ok(())
    }

    pub fn finalize(&self, outcome: TraceTerminalOutcome) -> Result<(), TraceProducerError> {
        self.ensure_current()?;
        let mut state = self.state.lock().expect("trace state lock");
        if state.terminal {
            return Err(TraceProducerError::AlreadyTerminal);
        }
        if !state.reserved_sequences.is_empty() {
            return Err(TraceProducerError::SequenceGap);
        }
        self.sink
            .terminalize(self.run_id, outcome)
            .map_err(|_| TraceProducerError::Sink)?;
        state.terminal = true;
        Ok(())
    }

    fn ensure_current(&self) -> Result<(), TraceProducerError> {
        match self.identity.verified_identity() {
            Some(current) if current == self.captured_identity => Ok(()),
            _ => Err(TraceProducerError::StaleIdentity),
        }
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

    use nwflash_domain::TraceOutputStreamV2;
    use nwflash_protection::{ExactSecretSet, TraceOutputSession};

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum SinkCall {
        Open(TraceId, OperationKind),
        Authorization(TraceId, TraceAuthorization),
        Upload(TraceId, u64),
        Terminal(TraceId, TraceTerminalOutcome),
    }

    #[derive(Default)]
    struct RecordingSink {
        calls: Mutex<Vec<SinkCall>>,
    }

    #[derive(Clone)]
    struct FixedIdentity {
        snapshot: Arc<Mutex<Option<TraceIdentitySnapshot>>>,
    }

    struct FixedAuthorizer {
        decision: Result<TraceAuthorization, TraceProducerError>,
    }

    impl TraceMetadataSink for RecordingSink {
        fn open_run(&self, run: &TraceRunOpen) -> Result<(), TraceProducerError> {
            self.calls
                .lock()
                .unwrap()
                .push(SinkCall::Open(run.run_id, run.operation));
            Ok(())
        }

        fn record_authorization(
            &self,
            run_id: TraceId,
            authorization: TraceAuthorization,
        ) -> Result<(), TraceProducerError> {
            self.calls
                .lock()
                .unwrap()
                .push(SinkCall::Authorization(run_id, authorization));
            Ok(())
        }

        fn append_upload(
            &self,
            run_id: TraceId,
            sequence: u64,
            _upload: SealedTraceUpload,
        ) -> Result<(), TraceProducerError> {
            self.calls
                .lock()
                .unwrap()
                .push(SinkCall::Upload(run_id, sequence));
            Ok(())
        }

        fn terminalize(
            &self,
            run_id: TraceId,
            outcome: TraceTerminalOutcome,
        ) -> Result<(), TraceProducerError> {
            self.calls
                .lock()
                .unwrap()
                .push(SinkCall::Terminal(run_id, outcome));
            Ok(())
        }
    }

    impl TraceIdentityProvider for FixedIdentity {
        fn verified_identity(&self) -> Option<TraceIdentitySnapshot> {
            self.snapshot.lock().unwrap().clone()
        }
    }

    impl TraceAuthorizationProvider for FixedAuthorizer {
        fn authorize(
            &self,
            _operation: OperationKind,
            _run_id: TraceId,
        ) -> BoxFuture<'static, Result<TraceAuthorization, TraceProducerError>> {
            let result = self.decision.clone();
            Box::pin(async move { result })
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

    fn sealed_upload() -> SealedTraceUpload {
        let mut reader = Cursor::new(b"safe trace output\n".to_vec());
        TraceOutputSession::from_reader(
            TraceId::try_new_v7().expect("event UUIDv7"),
            TraceOutputStreamV2::Stdout,
            &mut reader,
            &ExactSecretSet::empty(),
        )
        .expect("complete output stream")
        .into_upload_attempts()
        .expect("bounded sealed upload")
        .into_iter()
        .next()
        .expect("one sealed upload")
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
                SinkCall::Open(run.run_id(), OperationKind::Flashing),
                SinkCall::Authorization(run.run_id(), TraceAuthorization::Allowed),
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
            sink,
        );
        let run = producer.start(OperationKind::Discovering).await.unwrap();
        *identity_provider.snapshot.lock().unwrap() = Some(identity(5));
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

        run.append_upload(1, sealed_upload()).unwrap();
        assert!(matches!(
            run.append_upload(1, sealed_upload()),
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
            run.append_upload(2, sealed_upload()),
            Err(TraceProducerError::SequenceGap)
        ));
        assert!(!sink
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| matches!(call, SinkCall::Upload(_, 2))));

        run.append_upload(1, sealed_upload()).unwrap();
        run.finalize(TraceTerminalOutcome::Success).unwrap();
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
