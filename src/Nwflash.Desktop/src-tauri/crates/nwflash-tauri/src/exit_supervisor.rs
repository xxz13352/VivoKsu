use std::{
    panic::AssertUnwindSafe,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use futures::{future::BoxFuture, stream::FuturesUnordered, FutureExt, StreamExt};
use nwflash_application::{
    OperationCoordinator, OperationIdleLease, SessionLifecycle, SessionLifecycleError,
};
use nwflash_infrastructure::{IntegrityReportPhase, IntegrityReportReason};
use tokio::{
    sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender},
    time::{sleep_until, Instant},
};

use crate::integrity_reporter::{IntegrityEvent, IntegrityReportAuthorization, IntegrityReporter};

pub(crate) const TAMPER_CLOSEOUT_DEADLINE: Duration = Duration::from_millis(750);
const PROTECTED_EXIT_CODE: i32 = 70;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExitMode {
    Delayed,
    ImmediateTamper,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // Closed Worker allowlist; not every phase has a current producer.
pub(crate) enum ExitPhase {
    Startup,
    Login,
    SessionRestore,
    Heartbeat,
    OperationAdmission,
    PinValidation,
}

impl From<ExitPhase> for IntegrityReportPhase {
    fn from(value: ExitPhase) -> Self {
        match value {
            ExitPhase::Startup => Self::Startup,
            ExitPhase::Login => Self::Login,
            ExitPhase::SessionRestore => Self::SessionRestore,
            ExitPhase::Heartbeat => Self::Heartbeat,
            ExitPhase::OperationAdmission => Self::OperationAdmission,
            ExitPhase::PinValidation => Self::PinValidation,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IntegrityReason {
    ImageCrcInvalid,
    LeaseSignatureInvalid,
    LeaseBindingInvalid,
    LeaseExpired,
    SequenceRollback,
    PinMismatch,
}

impl From<IntegrityReason> for IntegrityReportReason {
    fn from(value: IntegrityReason) -> Self {
        match value {
            IntegrityReason::ImageCrcInvalid => Self::ImageCrcInvalid,
            IntegrityReason::LeaseSignatureInvalid => Self::LeaseSignatureInvalid,
            IntegrityReason::LeaseBindingInvalid => Self::LeaseBindingInvalid,
            IntegrityReason::LeaseExpired => Self::LeaseExpired,
            IntegrityReason::SequenceRollback => Self::SequenceRollback,
            IntegrityReason::PinMismatch => Self::PinMismatch,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExitReason {
    Integrity(IntegrityReason),
    ServerForced,
    SessionUnauthorized,
    SessionConflict,
    UpdateRequired,
    HeartbeatUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExitRequest {
    mode: ExitMode,
    phase: ExitPhase,
    reason: ExitReason,
    generation: Option<String>,
}

impl ExitRequest {
    pub(crate) fn delayed(generation: String, reason: ExitReason) -> Self {
        debug_assert!(!matches!(reason, ExitReason::Integrity(_)));
        Self {
            mode: ExitMode::Delayed,
            phase: ExitPhase::Heartbeat,
            reason,
            generation: Some(generation),
        }
    }

    pub(crate) fn immediate(
        generation: Option<String>,
        phase: ExitPhase,
        reason: IntegrityReason,
    ) -> Self {
        Self {
            mode: ExitMode::ImmediateTamper,
            phase,
            reason: ExitReason::Integrity(reason),
            generation,
        }
    }

    #[cfg(test)]
    pub(crate) fn mode(&self) -> ExitMode {
        self.mode
    }

    #[cfg(test)]
    pub(crate) fn reason(&self) -> ExitReason {
        self.reason
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExitRequestDisposition {
    Accepted,
    Coalesced,
    EscalatedToTamper,
    IgnoredStaleGeneration,
    AlreadyTerminating,
    ChannelFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AdmissionClosed;

pub(crate) trait ProcessTerminator: Send + Sync + 'static {
    fn terminate(&self, exit_code: i32);
}

#[cfg(not(test))]
pub(crate) struct ProductionProcessTerminator;

#[cfg(not(test))]
impl ProcessTerminator for ProductionProcessTerminator {
    fn terminate(&self, exit_code: i32) {
        std::process::exit(exit_code);
    }
}

pub(crate) trait ExitLifecycleCloseout: Send + Sync {
    fn close_until(&self, deadline: Instant, send_goodbye: bool) -> BoxFuture<'static, ()>;
}

impl ExitLifecycleCloseout for SessionLifecycle {
    fn close_until(&self, deadline: Instant, send_goodbye: bool) -> BoxFuture<'static, ()> {
        let lifecycle = self.clone();
        Box::pin(async move {
            match lifecycle
                .close_for_exit_with_policy(deadline, send_goodbye)
                .await
            {
                Ok(()) | Err(SessionLifecycleError::NotStarted) => {}
                Err(SessionLifecycleError::AlreadyRunning | SessionLifecycleError::Message(_)) => {}
            }
        })
    }
}

pub(crate) trait ExitUsageCloseout: Send + Sync {
    fn flush_until(&self, deadline: Instant) -> BoxFuture<'static, ()>;
}

pub(crate) trait ExitCleanup: Send + Sync {
    fn revoke_and_clear(&self, idle: &OperationIdleLease);
}

#[derive(Clone)]
pub(crate) struct ExitSupervisorHandle {
    control: Arc<ExitSupervisorControl>,
    sender: UnboundedSender<()>,
    terminator: Arc<dyn ProcessTerminator>,
}

pub(crate) struct ExitSupervisorWorker {
    control: Arc<ExitSupervisorControl>,
    receiver: UnboundedReceiver<()>,
    lifecycle: Arc<dyn ExitLifecycleCloseout>,
    usage: Arc<dyn ExitUsageCloseout>,
    cleanup: Arc<dyn ExitCleanup>,
    terminator: Arc<dyn ProcessTerminator>,
}

struct ExitSupervisorControl {
    state: Mutex<SupervisorControlState>,
    coordinator: OperationCoordinator,
    reporter: IntegrityReporter,
}

#[derive(Default)]
struct SupervisorControlState {
    active_generation: Option<String>,
    accepted: Option<AcceptedExit>,
    terminating: bool,
    accepting_upgrades: bool,
    terminator_called: bool,
}

#[derive(Clone)]
struct AcceptedExit {
    policy: AcceptedPolicy,
    send_goodbye: bool,
    authorization: IntegrityReportAuthorization,
}

#[derive(Clone)]
enum AcceptedPolicy {
    Delayed { _reason: ExitReason },
    Immediate { event: IntegrityEvent },
}

#[cfg(test)]
pub(crate) fn create_exit_supervisor(
    coordinator: OperationCoordinator,
    lifecycle: Arc<dyn ExitLifecycleCloseout>,
    usage: Arc<dyn ExitUsageCloseout>,
    reporter: IntegrityReporter,
    cleanup: Arc<dyn ExitCleanup>,
    terminator: Arc<dyn ProcessTerminator>,
) -> (ExitSupervisorHandle, ExitSupervisorWorker) {
    let (handle, receiver) = create_exit_supervisor_control(coordinator, reporter, terminator);
    let worker = build_exit_supervisor_worker(&handle, receiver, lifecycle, usage, cleanup);
    (handle, worker)
}

pub(crate) fn create_exit_supervisor_control(
    coordinator: OperationCoordinator,
    reporter: IntegrityReporter,
    terminator: Arc<dyn ProcessTerminator>,
) -> (ExitSupervisorHandle, UnboundedReceiver<()>) {
    let (sender, receiver) = unbounded_channel();
    let control = Arc::new(ExitSupervisorControl {
        state: Mutex::new(SupervisorControlState::default()),
        coordinator,
        reporter,
    });
    let handle = ExitSupervisorHandle {
        control: control.clone(),
        sender,
        terminator,
    };
    (handle, receiver)
}

pub(crate) fn build_exit_supervisor_worker(
    handle: &ExitSupervisorHandle,
    receiver: UnboundedReceiver<()>,
    lifecycle: Arc<dyn ExitLifecycleCloseout>,
    usage: Arc<dyn ExitUsageCloseout>,
    cleanup: Arc<dyn ExitCleanup>,
) -> ExitSupervisorWorker {
    ExitSupervisorWorker {
        control: handle.control.clone(),
        receiver,
        lifecycle,
        usage,
        cleanup,
        terminator: handle.terminator.clone(),
    }
}

impl ExitSupervisorHandle {
    pub(crate) fn install_generation(&self, generation: String) -> Result<(), AdmissionClosed> {
        let mut state = self.control.lock();
        if state.accepted.is_some()
            || state.terminating
            || state.terminator_called
            || self.control.coordinator.admission_state()
                != nwflash_application::OperationAdmissionState::Running
        {
            return Err(AdmissionClosed);
        }
        state.active_generation = Some(generation);
        Ok(())
    }

    pub(crate) fn clear_generation(&self, expected: &str) {
        let mut state = self.control.lock();
        if state.active_generation.as_deref() == Some(expected) {
            state.active_generation = None;
        }
    }

    pub(crate) fn request(&self, request: ExitRequest) -> ExitRequestDisposition {
        let (disposition, notify_worker) = {
            let mut state = self.control.lock();
            if state.terminator_called || (state.terminating && !state.accepting_upgrades) {
                return ExitRequestDisposition::AlreadyTerminating;
            }

            let same_generation = request
                .generation
                .as_deref()
                .is_some_and(|generation| state.active_generation.as_deref() == Some(generation));
            if request.mode == ExitMode::Delayed && !same_generation {
                return ExitRequestDisposition::IgnoredStaleGeneration;
            }

            let authorization = if same_generation {
                IntegrityReportAuthorization::CurrentSession
            } else {
                IntegrityReportAuthorization::Anonymous
            };
            let send_goodbye = same_generation;

            match state.accepted.as_mut() {
                None => {
                    let policy = match request.reason {
                        ExitReason::Integrity(reason) => AcceptedPolicy::Immediate {
                            event: self
                                .control
                                .reporter
                                .prepare_event(request.phase.into(), reason.into()),
                        },
                        reason => AcceptedPolicy::Delayed { _reason: reason },
                    };
                    state.accepted = Some(AcceptedExit {
                        policy,
                        send_goodbye,
                        authorization,
                    });
                    self.control.coordinator.request_exit_pending();
                    (ExitRequestDisposition::Accepted, true)
                }
                Some(accepted) => match (&accepted.policy, request.reason) {
                    (AcceptedPolicy::Delayed { .. }, ExitReason::Integrity(reason)) => {
                        let event = self
                            .control
                            .reporter
                            .prepare_event(request.phase.into(), reason.into());
                        accepted.policy = AcceptedPolicy::Immediate { event };
                        accepted.authorization = authorization;
                        accepted.send_goodbye = send_goodbye;
                        (ExitRequestDisposition::EscalatedToTamper, true)
                    }
                    _ => (ExitRequestDisposition::Coalesced, false),
                },
            }
        };

        if notify_worker && self.sender.send(()).is_err() {
            call_terminator_once(&self.control, &self.terminator);
            return ExitRequestDisposition::ChannelFailed;
        }
        disposition
    }
}

impl ExitSupervisorControl {
    fn lock(&self) -> MutexGuard<'_, SupervisorControlState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn accepted(&self) -> AcceptedExit {
        self.lock()
            .accepted
            .clone()
            .expect("exit worker is notified only after request acceptance")
    }
}

impl ExitSupervisorWorker {
    pub(crate) async fn run(mut self) {
        let result = AssertUnwindSafe(self.run_inner()).catch_unwind().await;
        if result.is_err() {
            call_terminator_once(&self.control, &self.terminator);
        }
    }

    async fn run_inner(&mut self) {
        if self.receiver.recv().await.is_none() {
            return;
        }

        let idle = self.control.coordinator.wait_until_idle().await;
        self.control
            .coordinator
            .begin_terminating(&idle)
            .expect("accepted exit must own an idle lease before terminating");
        {
            let mut state = self.control.lock();
            state.terminating = true;
            state.accepting_upgrades = true;
        }

        let deadline = Instant::now() + TAMPER_CLOSEOUT_DEADLINE;
        let initial = self.control.accepted();
        let mut lifecycle = self
            .lifecycle
            .close_until(deadline, initial.send_goodbye)
            .fuse();
        let mut lifecycle_done = false;
        let mut report_started = false;
        let mut auxiliary: FuturesUnordered<BoxFuture<'static, ()>> = FuturesUnordered::new();
        auxiliary.push(self.usage.flush_until(deadline));
        if let AcceptedPolicy::Immediate { event } = initial.policy {
            report_started = true;
            auxiliary.push(report_future(
                self.control.reporter.clone(),
                event,
                initial.authorization,
                deadline,
            ));
        }
        let deadline_sleep = sleep_until(deadline);
        tokio::pin!(deadline_sleep);

        loop {
            if lifecycle_done && auxiliary.is_empty() {
                break;
            }
            tokio::select! {
                _ = &mut deadline_sleep => break,
                _ = &mut lifecycle, if !lifecycle_done => {
                    lifecycle_done = true;
                }
                _ = auxiliary.next(), if !auxiliary.is_empty() => {}
                message = self.receiver.recv(), if !report_started => {
                    if message.is_none() {
                        continue;
                    }
                    let accepted = self.control.accepted();
                    if let AcceptedPolicy::Immediate { event } = accepted.policy {
                        report_started = true;
                        auxiliary.push(report_future(
                            self.control.reporter.clone(),
                            event,
                            accepted.authorization,
                            deadline,
                        ));
                    }
                }
            }
        }

        drop(auxiliary);
        if !lifecycle_done {
            lifecycle.await;
        }
        {
            let mut state = self.control.lock();
            state.accepting_upgrades = false;
        }
        self.cleanup.revoke_and_clear(&idle);
        drop(idle);
        call_terminator_once(&self.control, &self.terminator);
    }
}

fn report_future(
    reporter: IntegrityReporter,
    event: IntegrityEvent,
    authorization: IntegrityReportAuthorization,
    deadline: Instant,
) -> BoxFuture<'static, ()> {
    Box::pin(async move {
        let _ = reporter.report_once(&event, authorization, deadline).await;
    })
}

/// Exceptional fail-closed path used only when the worker channel has vanished
/// or the worker panics. Admission is already closed; production terminates
/// immediately because ordered network closeout can no longer be guaranteed.
fn call_terminator_once(
    control: &Arc<ExitSupervisorControl>,
    terminator: &Arc<dyn ProcessTerminator>,
) {
    let should_terminate = {
        let mut state = control.lock();
        state.terminating = true;
        state.accepting_upgrades = false;
        if state.terminator_called {
            false
        } else {
            state.terminator_called = true;
            true
        }
    };
    if should_terminate {
        terminator.terminate(PROTECTED_EXIT_CODE);
    }
}

#[cfg(test)]
mod tests {
    use std::{
        future::pending,
        sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            Arc, Barrier, Mutex, RwLock,
        },
        time::Duration,
    };

    use futures::future::BoxFuture;
    use nwflash_application::{
        OperationAdmissionState, OperationCoordinator, OperationCoordinatorError,
        OperationIdleLease,
    };
    use nwflash_domain::OperationKind;
    use nwflash_infrastructure::{
        CloudflareError, CloudflareResult, IntegrityReportRequest, SecretToken,
    };
    use tokio::{
        sync::Notify,
        time::{timeout_at, Instant},
    };

    use crate::integrity_reporter::{IntegrityReportClient, IntegrityReporter};

    use super::{
        create_exit_supervisor, AdmissionClosed, ExitCleanup, ExitLifecycleCloseout, ExitMode,
        ExitPhase, ExitReason, ExitRequest, ExitRequestDisposition, ExitUsageCloseout,
        IntegrityReason, ProcessTerminator,
    };

    #[derive(Default)]
    struct RecordingTerminator {
        calls: Mutex<Vec<i32>>,
        called: Notify,
        order: Arc<Mutex<Vec<&'static str>>>,
    }

    impl ProcessTerminator for RecordingTerminator {
        fn terminate(&self, exit_code: i32) {
            self.order.lock().unwrap().push("terminate");
            self.calls.lock().unwrap().push(exit_code);
            self.called.notify_waiters();
        }
    }

    struct FakeLifecycle {
        pending: bool,
        starts: AtomicUsize,
        deadlines: Mutex<Vec<Instant>>,
        goodbye: Mutex<Vec<bool>>,
        order: Arc<Mutex<Vec<&'static str>>>,
    }

    impl FakeLifecycle {
        fn new(pending: bool, order: Arc<Mutex<Vec<&'static str>>>) -> Self {
            Self {
                pending,
                starts: AtomicUsize::new(0),
                deadlines: Mutex::new(Vec::new()),
                goodbye: Mutex::new(Vec::new()),
                order,
            }
        }
    }

    impl ExitLifecycleCloseout for FakeLifecycle {
        fn close_until(&self, deadline: Instant, send_goodbye: bool) -> BoxFuture<'static, ()> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            self.deadlines.lock().unwrap().push(deadline);
            self.goodbye.lock().unwrap().push(send_goodbye);
            self.order.lock().unwrap().push("lifecycle-start");
            let is_pending = self.pending;
            let order = self.order.clone();
            Box::pin(async move {
                if is_pending {
                    let _ = timeout_at(deadline, pending::<()>()).await;
                }
                order.lock().unwrap().push("lifecycle-end");
            })
        }
    }

    struct FakeUsage {
        pending: bool,
        starts: AtomicUsize,
        deadlines: Mutex<Vec<Instant>>,
        order: Arc<Mutex<Vec<&'static str>>>,
    }

    impl FakeUsage {
        fn new(pending: bool, order: Arc<Mutex<Vec<&'static str>>>) -> Self {
            Self {
                pending,
                starts: AtomicUsize::new(0),
                deadlines: Mutex::new(Vec::new()),
                order,
            }
        }
    }

    impl ExitUsageCloseout for FakeUsage {
        fn flush_until(&self, deadline: Instant) -> BoxFuture<'static, ()> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            self.deadlines.lock().unwrap().push(deadline);
            self.order.lock().unwrap().push("usage-start");
            let is_pending = self.pending;
            Box::pin(async move {
                if is_pending {
                    pending::<()>().await;
                }
            })
        }
    }

    struct RecordingCleanup {
        calls: AtomicUsize,
        panic: bool,
        order: Arc<Mutex<Vec<&'static str>>>,
    }

    impl ExitCleanup for RecordingCleanup {
        fn revoke_and_clear(&self, _idle: &OperationIdleLease) {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.order.lock().unwrap().push("cleanup");
            assert!(!self.panic, "injected cleanup panic");
        }
    }

    #[derive(Clone, Copy)]
    enum ReportBehavior {
        Accepted,
        HttpError,
        Pending,
    }

    struct FakeReportClient {
        behavior: ReportBehavior,
        calls: Mutex<Vec<(bool, IntegrityReportRequest)>>,
    }

    impl IntegrityReportClient for FakeReportClient {
        fn report_integrity(
            &self,
            token: Option<SecretToken>,
            request: IntegrityReportRequest,
        ) -> BoxFuture<'static, CloudflareResult<()>> {
            self.calls.lock().unwrap().push((token.is_some(), request));
            match self.behavior {
                ReportBehavior::Accepted => Box::pin(async { Ok(()) }),
                ReportBehavior::HttpError => Box::pin(async {
                    Err(CloudflareError::ApiError {
                        status: 503,
                        message: "fixed".to_string(),
                    })
                }),
                ReportBehavior::Pending => Box::pin(pending()),
            }
        }
    }

    struct Harness {
        coordinator: OperationCoordinator,
        handle: super::ExitSupervisorHandle,
        worker: Option<super::ExitSupervisorWorker>,
        lifecycle: Arc<FakeLifecycle>,
        usage: Arc<FakeUsage>,
        cleanup: Arc<RecordingCleanup>,
        terminator: Arc<RecordingTerminator>,
        report_client: Arc<FakeReportClient>,
        reporter: IntegrityReporter,
        order: Arc<Mutex<Vec<&'static str>>>,
        _marker_root: tempfile::TempDir,
    }

    fn harness(
        report_behavior: ReportBehavior,
        lifecycle_pending: bool,
        usage_pending: bool,
        cleanup_panics: bool,
    ) -> Harness {
        let order = Arc::new(Mutex::new(Vec::new()));
        let coordinator = OperationCoordinator::default();
        let lifecycle = Arc::new(FakeLifecycle::new(lifecycle_pending, order.clone()));
        let usage = Arc::new(FakeUsage::new(usage_pending, order.clone()));
        let cleanup = Arc::new(RecordingCleanup {
            calls: AtomicUsize::new(0),
            panic: cleanup_panics,
            order: order.clone(),
        });
        let terminator = Arc::new(RecordingTerminator {
            calls: Mutex::new(Vec::new()),
            called: Notify::new(),
            order: order.clone(),
        });
        let report_client = Arc::new(FakeReportClient {
            behavior: report_behavior,
            calls: Mutex::new(Vec::new()),
        });
        let marker_root = tempfile::tempdir().unwrap();
        let reporter = IntegrityReporter::with_client(
            report_client.clone(),
            Arc::new(RwLock::new(Some(SecretToken::new(
                "same-generation".to_string(),
            )))),
            "1.0.1".to_string(),
            "build-test".to_string(),
            marker_root.path().to_path_buf(),
        );
        let (handle, worker) = create_exit_supervisor(
            coordinator.clone(),
            lifecycle.clone(),
            usage.clone(),
            reporter.clone(),
            cleanup.clone(),
            terminator.clone(),
        );
        Harness {
            coordinator,
            handle,
            worker: Some(worker),
            lifecycle,
            usage,
            cleanup,
            terminator,
            report_client,
            reporter,
            order,
            _marker_root: marker_root,
        }
    }

    fn delayed(generation: &str) -> ExitRequest {
        ExitRequest::delayed(generation.to_string(), ExitReason::ServerForced)
    }

    fn immediate(generation: Option<&str>) -> ExitRequest {
        ExitRequest::immediate(
            generation.map(str::to_string),
            ExitPhase::Heartbeat,
            IntegrityReason::LeaseSignatureInvalid,
        )
    }

    #[tokio::test]
    async fn active_operation_finishes_naturally_before_cleanup_and_termination() {
        let mut harness = harness(ReportBehavior::Accepted, false, false, false);
        harness
            .handle
            .install_generation("generation-a".to_string())
            .unwrap();
        let body_entered = Arc::new(Notify::new());
        let entered = body_entered.notified();
        let body_release = Arc::new(Notify::new());
        let cancellation_seen = Arc::new(AtomicBool::new(false));
        let operation = tokio::spawn({
            let coordinator = harness.coordinator.clone();
            let body_entered = body_entered.clone();
            let body_release = body_release.clone();
            let cancellation_seen = cancellation_seen.clone();
            async move {
                coordinator.run_async(OperationKind::Flashing, "active", move |_, token| async move {
                    body_entered.notify_one();
                    tokio::select! {
                        _ = token.cancelled() => cancellation_seen.store(true, Ordering::SeqCst),
                        _ = body_release.notified() => {}
                    }
                    Ok(())
                }).await
            }
        });
        entered.await;
        let worker = tokio::spawn(harness.worker.take().unwrap().run());

        assert_eq!(
            harness.handle.request(delayed("generation-a")),
            ExitRequestDisposition::Accepted
        );
        assert_eq!(
            harness.coordinator.admission_state(),
            OperationAdmissionState::ExitPending
        );
        assert!(matches!(
            harness
                .coordinator
                .run_async(OperationKind::Flashing, "late", |_, _| async { Ok(()) })
                .await,
            Err(OperationCoordinatorError::ExitPending)
        ));
        assert_eq!(harness.cleanup.calls.load(Ordering::SeqCst), 0);
        assert!(harness.terminator.calls.lock().unwrap().is_empty());
        assert!(!cancellation_seen.load(Ordering::SeqCst));

        body_release.notify_one();
        operation.await.unwrap().unwrap();
        worker.await.unwrap();
        assert!(!cancellation_seen.load(Ordering::SeqCst));
        assert_eq!(harness.cleanup.calls.load(Ordering::SeqCst), 1);
        assert_eq!(harness.terminator.calls.lock().unwrap().len(), 1);
        let order = harness.order.lock().unwrap().clone();
        assert!(
            order.iter().position(|step| *step == "cleanup").unwrap()
                < order.iter().position(|step| *step == "terminate").unwrap()
        );
    }

    #[test]
    fn stale_delayed_generation_is_ignored_and_login_race_is_linearizable() {
        let initial = harness(ReportBehavior::Accepted, false, false, false);
        initial
            .handle
            .install_generation("generation-a".to_string())
            .unwrap();
        initial.handle.clear_generation("generation-a");
        initial
            .handle
            .install_generation("generation-b".to_string())
            .unwrap();
        assert_eq!(
            initial.handle.request(delayed("generation-a")),
            ExitRequestDisposition::IgnoredStaleGeneration
        );
        assert_eq!(
            initial.coordinator.admission_state(),
            OperationAdmissionState::Running
        );

        for _ in 0..32 {
            let race = harness(ReportBehavior::Accepted, false, false, false);
            race.handle
                .install_generation("generation-a".to_string())
                .unwrap();
            let barrier = Arc::new(Barrier::new(3));
            let installer = std::thread::spawn({
                let handle = race.handle.clone();
                let barrier = barrier.clone();
                move || {
                    barrier.wait();
                    handle.install_generation("generation-b".to_string())
                }
            });
            let terminal = std::thread::spawn({
                let handle = race.handle.clone();
                let barrier = barrier.clone();
                move || {
                    barrier.wait();
                    handle.request(delayed("generation-a"))
                }
            });
            barrier.wait();
            let install = installer.join().unwrap();
            let terminal = terminal.join().unwrap();
            assert!(matches!(
                (install, terminal),
                (Ok(()), ExitRequestDisposition::IgnoredStaleGeneration)
                    | (Err(AdmissionClosed), ExitRequestDisposition::Accepted)
            ));
        }
    }

    #[tokio::test]
    async fn duplicates_coalesce_and_delayed_upgrades_once_without_downgrade() {
        let mut harness = harness(ReportBehavior::HttpError, false, false, false);
        harness
            .handle
            .install_generation("generation-a".to_string())
            .unwrap();
        assert_eq!(
            harness.handle.request(delayed("generation-a")),
            ExitRequestDisposition::Accepted
        );
        assert_eq!(
            harness.handle.request(delayed("generation-a")),
            ExitRequestDisposition::Coalesced
        );
        assert_eq!(
            harness.handle.request(immediate(Some("generation-a"))),
            ExitRequestDisposition::EscalatedToTamper
        );
        assert_eq!(
            harness.handle.request(delayed("generation-a")),
            ExitRequestDisposition::Coalesced
        );
        assert_eq!(
            harness.handle.request(immediate(Some("generation-a"))),
            ExitRequestDisposition::Coalesced
        );
        let next_event = harness.reporter.prepare_event(
            nwflash_infrastructure::IntegrityReportPhase::Heartbeat,
            nwflash_infrastructure::IntegrityReportReason::LeaseExpired,
        );
        assert!(
            next_event.request().event_id.ends_with("-1"),
            "coalesced immediate requests must not consume another event id"
        );

        harness.worker.take().unwrap().run().await;
        assert_eq!(harness.report_client.calls.lock().unwrap().len(), 1);
        assert_eq!(harness.lifecycle.starts.load(Ordering::SeqCst), 1);
        assert_eq!(harness.usage.starts.load(Ordering::SeqCst), 1);
        assert_eq!(harness.cleanup.calls.load(Ordering::SeqCst), 1);
        assert_eq!(harness.terminator.calls.lock().unwrap().len(), 1);
        assert_eq!(
            harness.lifecycle.goodbye.lock().unwrap().as_slice(),
            &[true]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn delayed_closeout_upgrade_starts_one_report_without_restarting_deadline() {
        let mut harness = harness(ReportBehavior::Accepted, true, true, false);
        harness
            .handle
            .install_generation("generation-a".to_string())
            .unwrap();
        let worker = tokio::spawn(harness.worker.take().unwrap().run());
        assert_eq!(
            harness.handle.request(delayed("generation-a")),
            ExitRequestDisposition::Accepted
        );
        while harness.lifecycle.starts.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        let original_deadline = harness.lifecycle.deadlines.lock().unwrap()[0];

        assert_eq!(
            harness.handle.request(immediate(Some("generation-a"))),
            ExitRequestDisposition::EscalatedToTamper
        );
        while harness.report_client.calls.lock().unwrap().is_empty() {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            harness.lifecycle.deadlines.lock().unwrap()[0],
            original_deadline
        );
        assert_eq!(harness.report_client.calls.lock().unwrap().len(), 1);

        tokio::time::advance(Duration::from_millis(750)).await;
        worker.await.unwrap();
        assert_eq!(harness.terminator.calls.lock().unwrap().len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn immediate_closeout_shares_one_750ms_deadline_for_report_goodbye_and_usage() {
        let mut harness = harness(ReportBehavior::Pending, true, true, false);
        harness
            .handle
            .install_generation("generation-a".to_string())
            .unwrap();
        let worker = tokio::spawn(harness.worker.take().unwrap().run());
        assert_eq!(
            harness.handle.request(immediate(Some("generation-a"))),
            ExitRequestDisposition::Accepted
        );

        while harness.report_client.calls.lock().unwrap().is_empty()
            || harness.lifecycle.starts.load(Ordering::SeqCst) == 0
            || harness.usage.starts.load(Ordering::SeqCst) == 0
        {
            tokio::task::yield_now().await;
        }
        let lifecycle_deadline = harness.lifecycle.deadlines.lock().unwrap()[0];
        let usage_deadline = harness.usage.deadlines.lock().unwrap()[0];
        assert_eq!(lifecycle_deadline, usage_deadline);
        tokio::time::advance(Duration::from_millis(749)).await;
        tokio::task::yield_now().await;
        assert!(harness.terminator.calls.lock().unwrap().is_empty());
        tokio::time::advance(Duration::from_millis(1)).await;

        worker.await.unwrap();
        assert_eq!(harness.report_client.calls.lock().unwrap().len(), 1);
        assert_eq!(harness.cleanup.calls.load(Ordering::SeqCst), 1);
        assert_eq!(harness.terminator.calls.lock().unwrap().len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn active_operation_wait_is_outside_immediate_tamper_deadline() {
        let mut harness = harness(ReportBehavior::Accepted, true, true, false);
        harness
            .handle
            .install_generation("generation-a".to_string())
            .unwrap();
        let body_entered = Arc::new(Notify::new());
        let entered = body_entered.notified();
        let (body_release_tx, body_release_rx) = tokio::sync::oneshot::channel();
        let operation = tokio::spawn({
            let coordinator = harness.coordinator.clone();
            let body_entered = body_entered.clone();
            async move {
                coordinator
                    .run_async(
                        OperationKind::Flashing,
                        "active-before-tamper",
                        move |_, _| async move {
                            body_entered.notify_one();
                            let _ = body_release_rx.await;
                            Ok(())
                        },
                    )
                    .await
            }
        });
        entered.await;
        let worker = tokio::spawn(harness.worker.take().unwrap().run());
        assert_eq!(
            harness.handle.request(immediate(Some("generation-a"))),
            ExitRequestDisposition::Accepted
        );

        tokio::time::advance(Duration::from_secs(5)).await;
        tokio::task::yield_now().await;
        assert_eq!(harness.lifecycle.starts.load(Ordering::SeqCst), 0);
        assert!(harness.terminator.calls.lock().unwrap().is_empty());

        let _ = body_release_tx.send(());
        operation.await.unwrap().unwrap();
        while harness.lifecycle.starts.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            harness.lifecycle.deadlines.lock().unwrap()[0].duration_since(Instant::now()),
            Duration::from_millis(750)
        );

        tokio::time::advance(Duration::from_millis(750)).await;
        worker.await.unwrap();
        assert_eq!(harness.terminator.calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn stale_integrity_is_anonymous_and_sends_no_new_generation_goodbye() {
        let mut harness = harness(ReportBehavior::Accepted, false, false, false);
        harness
            .handle
            .install_generation("generation-b".to_string())
            .unwrap();
        assert_eq!(
            harness.handle.request(immediate(Some("generation-a"))),
            ExitRequestDisposition::Accepted
        );
        harness.worker.take().unwrap().run().await;

        assert!(!harness.report_client.calls.lock().unwrap()[0].0);
        assert_eq!(
            harness.lifecycle.goodbye.lock().unwrap().as_slice(),
            &[false]
        );
    }

    #[test]
    fn closed_worker_channel_invokes_fail_closed_terminator_once_without_production_exit() {
        let mut harness = harness(ReportBehavior::Accepted, false, false, false);
        drop(harness.worker.take());
        assert_eq!(
            harness.handle.request(immediate(None)),
            ExitRequestDisposition::ChannelFailed
        );
        assert_eq!(harness.terminator.calls.lock().unwrap().len(), 1);
        assert_eq!(
            harness.handle.request(immediate(None)),
            ExitRequestDisposition::AlreadyTerminating
        );
        assert_eq!(harness.terminator.calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn worker_panic_and_returning_terminator_still_converge_exactly_once() {
        let mut harness = harness(ReportBehavior::Accepted, false, false, true);
        assert_eq!(
            harness.handle.request(immediate(None)),
            ExitRequestDisposition::Accepted
        );
        harness.worker.take().unwrap().run().await;
        assert_eq!(harness.terminator.calls.lock().unwrap().len(), 1);
        assert_eq!(
            harness.coordinator.admission_state(),
            OperationAdmissionState::Terminating
        );
        assert_eq!(
            harness.handle.request(immediate(None)),
            ExitRequestDisposition::AlreadyTerminating
        );
        assert_eq!(harness.terminator.calls.lock().unwrap().len(), 1);
    }

    #[test]
    fn immediate_constructor_cannot_be_downgraded_to_delayed_mode() {
        let request = immediate(None);
        assert_eq!(request.mode(), ExitMode::ImmediateTamper);
        assert!(matches!(request.reason(), ExitReason::Integrity(_)));
    }
}
