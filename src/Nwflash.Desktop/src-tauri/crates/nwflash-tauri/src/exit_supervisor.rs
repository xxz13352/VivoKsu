use std::{
    panic::{self, AssertUnwindSafe},
    sync::{Arc, Mutex},
    time::Duration,
};

use futures::{future::BoxFuture, FutureExt};
use nwflash_application::SessionLifecycle;
use nwflash_application::{OperationCoordinator, OperationIdleLease};
use nwflash_infrastructure::{IntegrityReportPhase, IntegrityReportReason};
use tokio::{
    sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender},
    time::{timeout_at, Instant},
};

pub const TAMPER_CLOSEOUT_DEADLINE: Duration = Duration::from_millis(750);
const MANDATORY_CLEANUP_RESERVE: Duration = Duration::from_millis(25);
const DELAYED_CLOSEOUT_DEADLINE: Duration = Duration::from_secs(3);
pub const PROTECTED_EXIT_CODE: i32 = 70;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitMode {
    Delayed,
    ImmediateTamper,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum ExitPhase {
    Startup,
    Login,
    SessionRestore,
    Heartbeat,
    OperationAdmission,
    PinValidation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrityReason {
    ImageCrcInvalid,
    LeaseSignatureInvalid,
    LeaseBindingInvalid,
    LeaseExpired,
    SequenceRollback,
    PinMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub enum ExitReason {
    Integrity(IntegrityReason),
    ServerForced,
    SessionUnauthorized,
    SessionConflict,
    UpdateRequired,
    HeartbeatUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExitRequest {
    pub mode: ExitMode,
    pub phase: ExitPhase,
    pub reason: ExitReason,
    pub generation: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitRequestDisposition {
    Accepted,
    Coalesced,
    EscalatedToTamper,
    IgnoredStaleGeneration,
    AlreadyTerminating,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmissionClosed;

pub trait ProcessTerminator: Send + Sync + 'static {
    fn terminate(&self, exit_code: i32);
}

pub struct ProductionProcessTerminator;

impl ProcessTerminator for ProductionProcessTerminator {
    fn terminate(&self, exit_code: i32) {
        std::process::exit(exit_code);
    }
}

pub trait ExitCleanup: Send + Sync + 'static {
    fn revoke_capability_and_clear_token(
        &self,
        idle: &OperationIdleLease,
    ) -> Vec<std::path::PathBuf>;

    fn cleanup_files(&self, paths: Vec<std::path::PathBuf>) {
        for path in paths {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

pub trait ExitCloseout: Send + Sync + 'static {
    fn closeout(
        &self,
        request: ExitRequest,
        deadline: Instant,
        authenticated_report: bool,
    ) -> BoxFuture<'static, ()>;
}

pub struct RuntimeExitCloseout {
    lifecycle: SessionLifecycle,
    usage_reporter: Arc<crate::usage_reporter::UsageLogReporter>,
    integrity_reporter: crate::integrity_reporter::IntegrityReporter,
}

impl RuntimeExitCloseout {
    pub fn new(
        lifecycle: SessionLifecycle,
        usage_reporter: Arc<crate::usage_reporter::UsageLogReporter>,
        integrity_reporter: crate::integrity_reporter::IntegrityReporter,
    ) -> Self {
        Self {
            lifecycle,
            usage_reporter,
            integrity_reporter,
        }
    }
}

impl ExitCloseout for RuntimeExitCloseout {
    fn closeout(
        &self,
        request: ExitRequest,
        deadline: Instant,
        authenticated_report: bool,
    ) -> BoxFuture<'static, ()> {
        let lifecycle = self.lifecycle.clone();
        let usage = self.usage_reporter.clone();
        let reporter = self.integrity_reporter.clone();
        async move {
            let goodbye = async move {
                let _ = lifecycle.stop_until(deadline).await;
            };
            let flush = async move {
                usage.flush().await;
            };
            let report = async move {
                if let ExitReason::Integrity(reason) = request.reason {
                    let phase = map_phase(request.phase);
                    let reason = map_integrity_reason(reason);
                    let _ = if authenticated_report {
                        reporter.report_once(phase, reason, deadline).await
                    } else {
                        reporter
                            .report_once_with_authentication(phase, reason, deadline, false)
                            .await
                    };
                }
            };
            tokio::join!(goodbye, flush, report);
        }
        .boxed()
    }
}

fn map_phase(value: ExitPhase) -> IntegrityReportPhase {
    match value {
        ExitPhase::Startup => IntegrityReportPhase::Startup,
        ExitPhase::Login => IntegrityReportPhase::Login,
        ExitPhase::SessionRestore => IntegrityReportPhase::SessionRestore,
        ExitPhase::Heartbeat => IntegrityReportPhase::Heartbeat,
        ExitPhase::OperationAdmission => IntegrityReportPhase::OperationAdmission,
        ExitPhase::PinValidation => IntegrityReportPhase::PinValidation,
    }
}

fn map_integrity_reason(value: IntegrityReason) -> IntegrityReportReason {
    match value {
        IntegrityReason::ImageCrcInvalid => IntegrityReportReason::ImageCrcInvalid,
        IntegrityReason::LeaseSignatureInvalid => IntegrityReportReason::LeaseSignatureInvalid,
        IntegrityReason::LeaseBindingInvalid => IntegrityReportReason::LeaseBindingInvalid,
        IntegrityReason::LeaseExpired => IntegrityReportReason::LeaseExpired,
        IntegrityReason::SequenceRollback => IntegrityReportReason::SequenceRollback,
        IntegrityReason::PinMismatch => IntegrityReportReason::PinMismatch,
    }
}

#[derive(Default)]
struct SupervisorControlState {
    active_generation: Option<String>,
    accepted: Option<AcceptedExit>,
    terminating: bool,
    terminator_called: bool,
}

#[derive(Clone)]
struct AcceptedExit {
    request: ExitRequest,
    authenticated_report: bool,
}

struct ExitSupervisorControl {
    state: Mutex<SupervisorControlState>,
    coordinator: OperationCoordinator,
    sender: UnboundedSender<()>,
}

#[derive(Clone)]
pub struct ExitSupervisorHandle {
    control: Arc<ExitSupervisorControl>,
}

impl ExitSupervisorHandle {
    pub fn install_generation(&self, generation: String) -> Result<(), AdmissionClosed> {
        let mut state = self
            .control
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.accepted.is_some() || state.terminating {
            return Err(AdmissionClosed);
        }
        state.active_generation = Some(generation);
        Ok(())
    }

    pub fn clear_generation(&self, expected: &str) {
        let mut state = self
            .control
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.active_generation.as_deref() == Some(expected) {
            state.active_generation = None;
        }
    }

    pub fn request(&self, request: ExitRequest) -> ExitRequestDisposition {
        let mut state = self
            .control
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.terminator_called {
            return ExitRequestDisposition::AlreadyTerminating;
        }
        let generation_matches = request
            .generation
            .as_deref()
            .is_none_or(|generation| Some(generation) == state.active_generation.as_deref());
        if request.mode == ExitMode::Delayed && !generation_matches {
            return ExitRequestDisposition::IgnoredStaleGeneration;
        }

        let running_worker = state.terminating;
        if let Some(accepted) = state.accepted.as_mut() {
            if accepted.request.mode == ExitMode::Delayed
                && request.mode == ExitMode::ImmediateTamper
            {
                *accepted = AcceptedExit {
                    request,
                    authenticated_report: generation_matches,
                };
                if running_worker {
                    let _ = self.control.sender.send(());
                }
                return ExitRequestDisposition::EscalatedToTamper;
            }
            if state.terminating || state.terminator_called {
                return ExitRequestDisposition::AlreadyTerminating;
            }
            return ExitRequestDisposition::Coalesced;
        }
        if state.terminating || state.terminator_called {
            return ExitRequestDisposition::AlreadyTerminating;
        }

        self.control.coordinator.request_exit_pending();
        state.accepted = Some(AcceptedExit {
            request,
            authenticated_report: generation_matches,
        });
        if self.control.sender.send(()).is_err() {
            state.terminating = true;
            return ExitRequestDisposition::AlreadyTerminating;
        }
        ExitRequestDisposition::Accepted
    }
}

pub struct ExitSupervisor;

pub struct ExitSupervisorWorker {
    control: Arc<ExitSupervisorControl>,
    receiver: UnboundedReceiver<()>,
    closeout: Arc<dyn ExitCloseout>,
    cleanup: Arc<dyn ExitCleanup>,
    terminator: Arc<dyn ProcessTerminator>,
}

impl ExitSupervisor {
    pub fn build(
        coordinator: OperationCoordinator,
        closeout: Arc<dyn ExitCloseout>,
        cleanup: Arc<dyn ExitCleanup>,
        terminator: Arc<dyn ProcessTerminator>,
    ) -> (ExitSupervisorHandle, ExitSupervisorWorker) {
        let (sender, receiver) = unbounded_channel();
        let control = Arc::new(ExitSupervisorControl {
            state: Mutex::new(SupervisorControlState::default()),
            coordinator,
            sender,
        });
        (
            ExitSupervisorHandle {
                control: control.clone(),
            },
            ExitSupervisorWorker {
                control,
                receiver,
                closeout,
                cleanup,
                terminator,
            },
        )
    }

    #[cfg(test)]
    pub fn spawn(
        coordinator: OperationCoordinator,
        closeout: Arc<dyn ExitCloseout>,
        cleanup: Arc<dyn ExitCleanup>,
        terminator: Arc<dyn ProcessTerminator>,
    ) -> ExitSupervisorHandle {
        let (handle, worker) = Self::build(coordinator, closeout, cleanup, terminator);
        tokio::spawn(worker.run());
        handle
    }
}

impl ExitSupervisorWorker {
    pub async fn run(mut self) {
        if self.receiver.recv().await.is_none() {
            return;
        }

        let idle = match self.control.coordinator.wait_until_idle().await {
            Ok(idle) => idle,
            Err(_) => return,
        };
        let request = {
            let mut state = self
                .control
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.terminating = true;
            state.accepted.clone()
        };
        let Some(mut accepted) = request else {
            return;
        };
        let _ = self.control.coordinator.begin_terminating(&idle);

        let mut tamper_handled = false;
        let mut closeout_exhausted = false;
        let mut final_deadline = loop {
            let budget = match accepted.request.mode {
                ExitMode::Delayed => DELAYED_CLOSEOUT_DEADLINE,
                ExitMode::ImmediateTamper => TAMPER_CLOSEOUT_DEADLINE,
            };
            let termination_deadline = Instant::now() + budget;
            let closeout_deadline = if accepted.request.mode == ExitMode::ImmediateTamper {
                termination_deadline - MANDATORY_CLEANUP_RESERVE
            } else {
                termination_deadline
            };
            let closeout = AssertUnwindSafe(self.closeout.closeout(
                accepted.request.clone(),
                closeout_deadline,
                accepted.authenticated_report,
            ))
            .catch_unwind();

            if accepted.request.mode == ExitMode::ImmediateTamper {
                closeout_exhausted = timeout_at(closeout_deadline, closeout).await.is_err();
                tamper_handled = true;
                break termination_deadline;
            }

            tokio::select! {
                _ = timeout_at(closeout_deadline, closeout) => {
                    let upgraded = self.control.state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .accepted
                        .clone();
                    if let Some(upgraded) = upgraded {
                        if upgraded.request.mode == ExitMode::ImmediateTamper {
                            accepted = upgraded;
                            continue;
                        }
                    }
                    break termination_deadline;
                },
                signal = self.receiver.recv() => {
                    if signal.is_none() {
                        break termination_deadline;
                    }
                    let upgraded = self.control.state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .accepted
                        .clone();
                    if let Some(upgraded) = upgraded {
                        accepted = upgraded;
                    }
                }
            }
        };

        let cleanup = self.cleanup.clone();
        let mut cleanup_task = tokio::task::spawn_blocking(move || {
            let paths = panic::catch_unwind(AssertUnwindSafe(|| {
                cleanup.revoke_capability_and_clear_token(&idle)
            }))
            .unwrap_or_default();
            drop(idle);
            cleanup.cleanup_files(paths);
        });

        let cleanup_completed = loop {
            tokio::select! {
                _ = &mut cleanup_task => break true,
                signal = self.receiver.recv() => {
                    if signal.is_none() || tamper_handled {
                        continue;
                    }
                    let upgraded = self.control.state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .accepted
                        .clone();
                    if let Some(upgraded) = upgraded {
                        if upgraded.request.mode == ExitMode::ImmediateTamper {
                            let termination_deadline = Instant::now() + TAMPER_CLOSEOUT_DEADLINE;
                            let closeout_deadline =
                                termination_deadline - MANDATORY_CLEANUP_RESERVE;
                            let closeout = AssertUnwindSafe(self.closeout.closeout(
                                upgraded.request,
                                closeout_deadline,
                                upgraded.authenticated_report,
                            ))
                            .catch_unwind();
                            closeout_exhausted =
                                timeout_at(closeout_deadline, closeout).await.is_err();
                            tamper_handled = true;
                            final_deadline = termination_deadline;
                        }
                    }
                }
                _ = tokio::time::sleep_until(final_deadline) => break false,
            }
        };

        loop {
            let unhandled_tamper = {
                let mut state = self
                    .control
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if state.terminator_called {
                    return;
                }
                let unhandled_tamper = (!tamper_handled)
                    .then(|| state.accepted.clone())
                    .flatten()
                    .filter(|accepted| accepted.request.mode == ExitMode::ImmediateTamper);
                if unhandled_tamper.is_none() {
                    state.terminator_called = true;
                }
                unhandled_tamper
            };
            if let Some(upgraded) = unhandled_tamper {
                let termination_deadline = Instant::now() + TAMPER_CLOSEOUT_DEADLINE;
                let closeout_deadline = termination_deadline - MANDATORY_CLEANUP_RESERVE;
                let closeout = AssertUnwindSafe(self.closeout.closeout(
                    upgraded.request,
                    closeout_deadline,
                    upgraded.authenticated_report,
                ))
                .catch_unwind();
                closeout_exhausted = timeout_at(closeout_deadline, closeout).await.is_err();
                final_deadline = termination_deadline;
                tamper_handled = true;
                continue;
            }
            break;
        }
        if cleanup_completed && closeout_exhausted && Instant::now() < final_deadline {
            tokio::time::sleep_until(final_deadline).await;
        }
        self.terminator.terminate(PROTECTED_EXIT_CODE);
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, Mutex},
        time::Duration,
    };

    use futures::future::{BoxFuture, FutureExt};
    use nwflash_application::{
        OperationAdmissionState, OperationCoordinator, OperationCoordinatorError,
    };
    use nwflash_domain::OperationKind;
    use tokio::sync::{oneshot, Notify};

    use super::*;

    #[derive(Clone)]
    struct RecordingCloseout {
        events: Arc<Mutex<Vec<&'static str>>>,
        pending: bool,
    }

    impl ExitCloseout for RecordingCloseout {
        fn closeout(
            &self,
            request: ExitRequest,
            _deadline: Instant,
            authenticated_report: bool,
        ) -> BoxFuture<'static, ()> {
            self.events
                .lock()
                .unwrap()
                .push(match (request.mode, authenticated_report) {
                    (ExitMode::Delayed, _) => "delayed-closeout",
                    (ExitMode::ImmediateTamper, true) => "tamper-closeout",
                    (ExitMode::ImmediateTamper, false) => "tamper-anonymous",
                });
            if self.pending {
                futures::future::pending().boxed()
            } else {
                futures::future::ready(()).boxed()
            }
        }
    }

    struct RecordingCleanup {
        events: Arc<Mutex<Vec<&'static str>>>,
    }

    impl ExitCleanup for RecordingCleanup {
        fn revoke_capability_and_clear_token(
            &self,
            _idle: &nwflash_application::OperationIdleLease,
        ) -> Vec<std::path::PathBuf> {
            self.events.lock().unwrap().push("cleanup");
            Vec::new()
        }
    }

    struct BlockingFileCleanup {
        events: Arc<Mutex<Vec<&'static str>>>,
        started: Arc<Notify>,
        release: Mutex<std::sync::mpsc::Receiver<()>>,
    }

    struct BlockingMandatoryCleanup {
        events: Arc<Mutex<Vec<&'static str>>>,
        started: Arc<Notify>,
        release: Mutex<std::sync::mpsc::Receiver<()>>,
    }

    impl ExitCleanup for BlockingMandatoryCleanup {
        fn revoke_capability_and_clear_token(
            &self,
            _idle: &nwflash_application::OperationIdleLease,
        ) -> Vec<std::path::PathBuf> {
            self.started.notify_waiters();
            let _ = self
                .release
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(2));
            self.events.lock().unwrap().push("cleanup");
            Vec::new()
        }
    }

    impl ExitCleanup for BlockingFileCleanup {
        fn revoke_capability_and_clear_token(
            &self,
            _idle: &nwflash_application::OperationIdleLease,
        ) -> Vec<std::path::PathBuf> {
            self.events.lock().unwrap().push("cleanup");
            vec![std::path::PathBuf::from("blocking-cleanup-sentinel")]
        }

        fn cleanup_files(&self, _paths: Vec<std::path::PathBuf>) {
            self.started.notify_waiters();
            let _ = self.release.lock().unwrap().recv();
        }
    }

    struct RecordingTerminator {
        events: Arc<Mutex<Vec<&'static str>>>,
        called: Arc<Notify>,
    }

    struct EscalatingCloseout {
        events: Arc<Mutex<Vec<&'static str>>>,
        delayed_started: Arc<Notify>,
    }

    impl ExitCloseout for EscalatingCloseout {
        fn closeout(
            &self,
            request: ExitRequest,
            _deadline: Instant,
            _authenticated_report: bool,
        ) -> BoxFuture<'static, ()> {
            match request.mode {
                ExitMode::Delayed => {
                    self.events.lock().unwrap().push("delayed-closeout");
                    self.delayed_started.notify_waiters();
                    futures::future::pending().boxed()
                }
                ExitMode::ImmediateTamper => {
                    self.events.lock().unwrap().push("tamper-closeout");
                    futures::future::ready(()).boxed()
                }
            }
        }
    }

    impl ProcessTerminator for RecordingTerminator {
        fn terminate(&self, exit_code: i32) {
            assert_eq!(exit_code, PROTECTED_EXIT_CODE);
            self.events.lock().unwrap().push("terminate");
            self.called.notify_waiters();
        }
    }

    fn harness(
        coordinator: OperationCoordinator,
        pending_closeout: bool,
    ) -> (
        ExitSupervisorHandle,
        Arc<Mutex<Vec<&'static str>>>,
        Arc<Notify>,
    ) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let called = Arc::new(Notify::new());
        let handle = ExitSupervisor::spawn(
            coordinator,
            Arc::new(RecordingCloseout {
                events: events.clone(),
                pending: pending_closeout,
            }),
            Arc::new(RecordingCleanup {
                events: events.clone(),
            }),
            Arc::new(RecordingTerminator {
                events: events.clone(),
                called: called.clone(),
            }),
        );
        (handle, events, called)
    }

    fn delayed(generation: &str) -> ExitRequest {
        ExitRequest {
            mode: ExitMode::Delayed,
            phase: ExitPhase::Heartbeat,
            reason: ExitReason::ServerForced,
            generation: Some(generation.to_string()),
        }
    }

    fn tamper() -> ExitRequest {
        ExitRequest {
            mode: ExitMode::ImmediateTamper,
            phase: ExitPhase::Heartbeat,
            reason: ExitReason::Integrity(IntegrityReason::LeaseSignatureInvalid),
            generation: None,
        }
    }

    #[tokio::test]
    async fn idle_delayed_exit_closes_admission_then_cleans_and_terminates_once() {
        let coordinator = OperationCoordinator::default();
        let (handle, events, terminated) = harness(coordinator.clone(), false);
        handle
            .install_generation("generation-a".to_string())
            .expect("running supervisor should accept generation");
        let terminated_wait = terminated.notified();

        assert_eq!(
            handle.request(delayed("generation-a")),
            ExitRequestDisposition::Accepted
        );
        assert_eq!(
            coordinator.admission_state(),
            OperationAdmissionState::ExitPending
        );
        tokio::time::timeout(Duration::from_secs(1), terminated_wait)
            .await
            .expect("idle supervisor should terminate");

        assert_eq!(
            events.lock().unwrap().as_slice(),
            ["delayed-closeout", "cleanup", "terminate"]
        );
        assert_eq!(
            coordinator.admission_state(),
            OperationAdmissionState::Terminating
        );
        assert_eq!(
            handle.request(delayed("generation-a")),
            ExitRequestDisposition::AlreadyTerminating
        );
    }

    #[tokio::test]
    async fn active_exit_waits_for_natural_completion_and_never_cancels_operation() {
        let coordinator = OperationCoordinator::default();
        let (release_tx, release_rx) = oneshot::channel::<()>();
        let (token_tx, token_rx) = oneshot::channel();
        let operation = tokio::spawn({
            let coordinator = coordinator.clone();
            async move {
                coordinator
                    .run_async(
                        OperationKind::Flashing,
                        "protected-operation",
                        move |_, token| async move {
                            token_tx.send(token).unwrap();
                            release_rx.await.unwrap();
                            Ok(())
                        },
                    )
                    .await
            }
        });
        let cancellation = token_rx.await.unwrap();
        let (handle, events, terminated) = harness(coordinator.clone(), false);
        handle
            .install_generation("generation-a".to_string())
            .unwrap();
        let terminated_wait = terminated.notified();

        assert_eq!(
            handle.request(delayed("generation-a")),
            ExitRequestDisposition::Accepted
        );
        assert!(matches!(
            coordinator
                .run_async(OperationKind::Flashing, "new-operation", |_, _| async {
                    Ok(())
                })
                .await,
            Err(OperationCoordinatorError::ExitPending)
        ));
        tokio::task::yield_now().await;
        assert!(events.lock().unwrap().is_empty());
        assert!(!cancellation.is_cancelled());

        release_tx.send(()).unwrap();
        operation.await.unwrap().unwrap();
        tokio::time::timeout(Duration::from_secs(1), terminated_wait)
            .await
            .expect("supervisor should terminate after operation releases lease");
        assert!(!cancellation.is_cancelled());
        assert_eq!(
            events.lock().unwrap().as_slice(),
            ["delayed-closeout", "cleanup", "terminate"]
        );
    }

    #[tokio::test]
    async fn stale_delayed_generation_is_ignored_without_closing_admission() {
        let coordinator = OperationCoordinator::default();
        let (handle, events, _) = harness(coordinator.clone(), false);
        handle
            .install_generation("generation-b".to_string())
            .unwrap();

        assert_eq!(
            handle.request(delayed("generation-a")),
            ExitRequestDisposition::IgnoredStaleGeneration
        );
        assert_eq!(
            coordinator.admission_state(),
            OperationAdmissionState::Running
        );
        assert!(events.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn stale_generation_tamper_terminates_with_anonymous_closeout() {
        let coordinator = OperationCoordinator::default();
        let (handle, events, terminated) = harness(coordinator, false);
        handle
            .install_generation("generation-b".to_string())
            .unwrap();
        let terminated_wait = terminated.notified();

        let mut stale_tamper = tamper();
        stale_tamper.generation = Some("generation-a".to_string());
        assert_eq!(
            handle.request(stale_tamper),
            ExitRequestDisposition::Accepted
        );
        tokio::time::timeout(Duration::from_secs(1), terminated_wait)
            .await
            .expect("stale tamper must still terminate process-wide");

        assert_eq!(events.lock().unwrap()[0], "tamper-anonymous");
    }

    #[tokio::test]
    async fn clear_generation_only_clears_the_exact_installed_generation() {
        let coordinator = OperationCoordinator::default();
        let (handle, _, _) = harness(coordinator, false);
        handle
            .install_generation("generation-b".to_string())
            .unwrap();
        handle.clear_generation("generation-a");
        assert_eq!(
            handle.request(delayed("generation-b")),
            ExitRequestDisposition::Accepted
        );

        let coordinator = OperationCoordinator::default();
        let (handle, _, _) = harness(coordinator.clone(), false);
        handle
            .install_generation("generation-b".to_string())
            .unwrap();
        handle.clear_generation("generation-b");
        assert_eq!(
            handle.request(delayed("generation-b")),
            ExitRequestDisposition::IgnoredStaleGeneration
        );
        assert_eq!(
            coordinator.admission_state(),
            OperationAdmissionState::Running
        );
    }

    #[tokio::test]
    async fn delayed_request_upgrades_to_one_immediate_tamper_closeout_before_idle() {
        let coordinator = OperationCoordinator::default();
        let (release_tx, release_rx) = oneshot::channel::<()>();
        let (entered_tx, entered_rx) = oneshot::channel();
        let operation = tokio::spawn({
            let coordinator = coordinator.clone();
            async move {
                coordinator
                    .run_async(OperationKind::Flashing, "held", move |_, _| async move {
                        entered_tx.send(()).unwrap();
                        release_rx.await.unwrap();
                        Ok(())
                    })
                    .await
            }
        });
        entered_rx.await.unwrap();
        let (handle, events, terminated) = harness(coordinator, false);
        handle
            .install_generation("generation-a".to_string())
            .unwrap();
        let terminated_wait = terminated.notified();

        assert_eq!(
            handle.request(delayed("generation-a")),
            ExitRequestDisposition::Accepted
        );
        assert_eq!(
            handle.request(tamper()),
            ExitRequestDisposition::EscalatedToTamper
        );
        release_tx.send(()).unwrap();
        operation.await.unwrap().unwrap();
        tokio::time::timeout(Duration::from_secs(1), terminated_wait)
            .await
            .expect("upgraded request should terminate");

        assert_eq!(events.lock().unwrap()[0], "tamper-closeout");
        assert_eq!(events.lock().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn tamper_after_delayed_closeout_start_cancels_and_tightens_worker() {
        let coordinator = OperationCoordinator::default();
        let events = Arc::new(Mutex::new(Vec::new()));
        let delayed_started = Arc::new(Notify::new());
        let terminated = Arc::new(Notify::new());
        let (handle, worker) = ExitSupervisor::build(
            coordinator,
            Arc::new(EscalatingCloseout {
                events: events.clone(),
                delayed_started: delayed_started.clone(),
            }),
            Arc::new(RecordingCleanup {
                events: events.clone(),
            }),
            Arc::new(RecordingTerminator {
                events: events.clone(),
                called: terminated.clone(),
            }),
        );
        tokio::spawn(worker.run());
        handle
            .install_generation("generation-a".to_string())
            .unwrap();
        let closeout_started = delayed_started.notified();
        let termination = terminated.notified();

        assert_eq!(
            handle.request(delayed("generation-a")),
            ExitRequestDisposition::Accepted
        );
        tokio::time::timeout(Duration::from_secs(1), closeout_started)
            .await
            .expect("delayed closeout should start");
        assert_eq!(
            handle.request(tamper()),
            ExitRequestDisposition::EscalatedToTamper
        );
        tokio::time::timeout(Duration::from_secs(1), termination)
            .await
            .expect("late tamper must tighten the running worker");

        assert_eq!(
            events.lock().unwrap().as_slice(),
            [
                "delayed-closeout",
                "tamper-closeout",
                "cleanup",
                "terminate"
            ]
        );
    }

    #[tokio::test]
    async fn pending_tamper_closeout_is_abandoned_at_one_750_ms_deadline() {
        let coordinator = OperationCoordinator::default();
        let (handle, events, terminated) = harness(coordinator, true);
        let terminated_wait = terminated.notified();
        let started = tokio::time::Instant::now();

        assert_eq!(handle.request(tamper()), ExitRequestDisposition::Accepted);
        tokio::time::timeout(Duration::from_secs(2), terminated_wait)
            .await
            .expect("timeout must not suppress termination");

        assert!(started.elapsed() >= TAMPER_CLOSEOUT_DEADLINE);
        assert!(started.elapsed() < Duration::from_millis(1_500));
        assert_eq!(
            events.lock().unwrap().as_slice(),
            ["tamper-closeout", "cleanup", "terminate"]
        );
    }

    #[tokio::test]
    async fn blocked_file_cleanup_cannot_extend_tamper_deadline() {
        let coordinator = OperationCoordinator::default();
        let events = Arc::new(Mutex::new(Vec::new()));
        let cleanup_started = Arc::new(Notify::new());
        let terminated = Arc::new(Notify::new());
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let (handle, worker) = ExitSupervisor::build(
            coordinator,
            Arc::new(RecordingCloseout {
                events: events.clone(),
                pending: false,
            }),
            Arc::new(BlockingFileCleanup {
                events: events.clone(),
                started: cleanup_started.clone(),
                release: Mutex::new(release_rx),
            }),
            Arc::new(RecordingTerminator {
                events: events.clone(),
                called: terminated.clone(),
            }),
        );
        tokio::spawn(worker.run());
        let cleanup_wait = cleanup_started.notified();
        let termination_wait = terminated.notified();
        let started = Instant::now();

        assert_eq!(handle.request(tamper()), ExitRequestDisposition::Accepted);
        tokio::time::timeout(Duration::from_secs(1), cleanup_wait)
            .await
            .expect("blocking cleanup should start off the supervisor task");
        tokio::time::timeout(Duration::from_secs(1), termination_wait)
            .await
            .expect("blocking cleanup must not suppress termination");

        assert!(started.elapsed() < Duration::from_millis(900));
        assert_eq!(
            events.lock().unwrap().as_slice(),
            ["tamper-closeout", "cleanup", "terminate"]
        );
        release_tx.send(()).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn tamper_after_delayed_closeout_completion_during_cleanup_is_reported() {
        let coordinator = OperationCoordinator::default();
        let events = Arc::new(Mutex::new(Vec::new()));
        let cleanup_started = Arc::new(Notify::new());
        let terminated = Arc::new(Notify::new());
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let (handle, worker) = ExitSupervisor::build(
            coordinator,
            Arc::new(RecordingCloseout {
                events: events.clone(),
                pending: false,
            }),
            Arc::new(BlockingMandatoryCleanup {
                events: events.clone(),
                started: cleanup_started.clone(),
                release: Mutex::new(release_rx),
            }),
            Arc::new(RecordingTerminator {
                events: events.clone(),
                called: terminated.clone(),
            }),
        );
        tokio::spawn(worker.run());
        handle
            .install_generation("generation-a".to_string())
            .unwrap();
        let cleanup_wait = cleanup_started.notified();
        let termination_wait = terminated.notified();

        assert_eq!(
            handle.request(delayed("generation-a")),
            ExitRequestDisposition::Accepted
        );
        tokio::time::timeout(Duration::from_secs(1), cleanup_wait)
            .await
            .expect("mandatory cleanup should start after delayed closeout completes");
        assert_eq!(
            handle.request(tamper()),
            ExitRequestDisposition::EscalatedToTamper
        );
        release_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(1), termination_wait)
            .await
            .expect("late tamper should be reported before termination");

        let events = events.lock().unwrap();
        assert_eq!(events.first(), Some(&"delayed-closeout"));
        assert_eq!(events.last(), Some(&"terminate"));
        assert!(events.contains(&"tamper-closeout"));
        assert!(events.contains(&"cleanup"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn blocked_mandatory_revocation_cannot_extend_tamper_deadline() {
        let coordinator = OperationCoordinator::default();
        let events = Arc::new(Mutex::new(Vec::new()));
        let cleanup_started = Arc::new(Notify::new());
        let terminated = Arc::new(Notify::new());
        let (_release_tx, release_rx) = std::sync::mpsc::channel();
        let (handle, worker) = ExitSupervisor::build(
            coordinator,
            Arc::new(RecordingCloseout {
                events: events.clone(),
                pending: false,
            }),
            Arc::new(BlockingMandatoryCleanup {
                events: events.clone(),
                started: cleanup_started.clone(),
                release: Mutex::new(release_rx),
            }),
            Arc::new(RecordingTerminator {
                events,
                called: terminated.clone(),
            }),
        );
        tokio::spawn(worker.run());
        let cleanup_wait = cleanup_started.notified();
        let termination_wait = terminated.notified();
        let started = Instant::now();

        assert_eq!(handle.request(tamper()), ExitRequestDisposition::Accepted);
        tokio::time::timeout(Duration::from_secs(1), cleanup_wait)
            .await
            .expect("mandatory revocation should start");
        tokio::time::timeout(Duration::from_secs(1), termination_wait)
            .await
            .expect("contended mandatory revocation must not suppress termination");
        assert!(started.elapsed() < Duration::from_millis(900));
    }
}
