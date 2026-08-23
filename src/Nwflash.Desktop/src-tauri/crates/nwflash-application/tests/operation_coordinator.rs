use std::{
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use futures::future::{BoxFuture, FutureExt};
use nwflash_application::{
    result_to_domain_error, OperationAdmissionState, OperationAuthorization, OperationContext,
    OperationCoordinator, OperationCoordinatorError, OperationPermissionGate, UsageReporter,
};
use nwflash_domain::{DomainError, OperationKind, PartitionTaskState, UsageLogEntry};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Default)]
struct Recorder {
    logs: Arc<std::sync::Mutex<Vec<String>>>,
}

impl UsageReporter for Recorder {
    fn record(&self, entry: UsageLogEntry) {
        self.logs.lock().unwrap().push(entry.status);
    }
}

#[derive(Clone, Default)]
struct UsageEntryRecorder {
    logs: Arc<std::sync::Mutex<Vec<UsageLogEntry>>>,
}

impl UsageReporter for UsageEntryRecorder {
    fn record(&self, entry: UsageLogEntry) {
        self.logs.lock().unwrap().push(entry);
    }
}

impl UsageEntryRecorder {
    fn statuses(&self) -> Vec<String> {
        self.logs
            .lock()
            .unwrap()
            .iter()
            .map(|entry| entry.status.clone())
            .collect()
    }

    fn event_ids(&self) -> Vec<String> {
        self.logs
            .lock()
            .unwrap()
            .iter()
            .map(|entry| entry.event_id.clone())
            .collect()
    }
}

#[derive(Clone, Default)]
struct TestLogger {
    stage: Arc<std::sync::Mutex<Vec<String>>>,
}

impl TestLogger {
    fn entries(&self) -> Vec<String> {
        self.stage.lock().unwrap().clone()
    }
}

impl nwflash_application::OperationLogger for TestLogger {
    fn write(
        &self,
        _level: nwflash_domain::OperationLogLevel,
        message: String,
        _operation_id: Option<String>,
    ) {
        self.stage.lock().unwrap().push(message);
    }
}

#[derive(Clone, Default)]
struct OperationIdLogger {
    entries: Arc<std::sync::Mutex<Vec<OperationLogRecord>>>,
}

type OperationLogRecord = (String, Option<String>);

impl OperationIdLogger {
    fn entries(&self) -> Vec<OperationLogRecord> {
        self.entries.lock().unwrap().clone()
    }
}

impl nwflash_application::OperationLogger for OperationIdLogger {
    fn write(
        &self,
        _level: nwflash_domain::OperationLogLevel,
        message: String,
        operation_id: Option<String>,
    ) {
        self.entries.lock().unwrap().push((message, operation_id));
    }
}

#[derive(Clone)]
struct PermissionGate {
    allowed: bool,
    reason: Option<String>,
}

impl PermissionGate {
    fn allow() -> Arc<Self> {
        Arc::new(Self {
            allowed: true,
            reason: None,
        })
    }

    fn deny(reason: &str) -> Arc<Self> {
        Arc::new(Self {
            allowed: false,
            reason: Some(reason.to_string()),
        })
    }
}

impl OperationPermissionGate for PermissionGate {
    fn authorize(
        &self,
        _operation: OperationKind,
        _title: String,
    ) -> BoxFuture<'static, Result<OperationAuthorization, DomainError>> {
        if self.allowed {
            futures::future::ready(Ok(OperationAuthorization::allow())).boxed()
        } else {
            let reason = self.reason.clone().unwrap_or_else(|| "禁止".to_string());
            futures::future::ready(Ok(OperationAuthorization::deny(reason))).boxed()
        }
    }
}

#[derive(Clone, Default)]
struct BlockingPermissionGate {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[derive(Clone, Default)]
struct CountingPermissionGate {
    calls: Arc<AtomicUsize>,
}

impl OperationPermissionGate for CountingPermissionGate {
    fn authorize(
        &self,
        _operation: OperationKind,
        _title: String,
    ) -> BoxFuture<'static, Result<OperationAuthorization, DomainError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        futures::future::ready(Ok(OperationAuthorization::allow())).boxed()
    }
}

#[tokio::test]
async fn exit_pending_rejects_before_permission_or_operation_body() {
    let gate = CountingPermissionGate::default();
    let body_calls = Arc::new(AtomicUsize::new(0));
    let coordinator =
        OperationCoordinator::new(None, Some(Arc::new(gate.clone())), None, None, None);

    assert_eq!(
        coordinator.request_exit_pending(),
        OperationAdmissionState::ExitPending
    );

    let body_calls_for_run = body_calls.clone();
    let result = coordinator
        .run_async(OperationKind::Flashing, "rejected", move |_, _| {
            body_calls_for_run.fetch_add(1, Ordering::SeqCst);
            async { Ok(()) }
        })
        .await;

    assert!(matches!(
        result,
        Err(OperationCoordinatorError::ExitPending)
    ));
    assert_eq!(gate.calls.load(Ordering::SeqCst), 0);
    assert_eq!(body_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn exit_pending_during_permission_wait_rechecks_before_operation_body() {
    let gate = BlockingPermissionGate::default();
    let authorization_entered = gate.entered.notified();
    let body_called = Arc::new(AtomicBool::new(false));
    let coordinator =
        OperationCoordinator::new(None, Some(Arc::new(gate.clone())), None, None, None);

    let operation = tokio::spawn({
        let coordinator = coordinator.clone();
        let body_called = body_called.clone();
        async move {
            coordinator
                .run_async(OperationKind::Flashing, "permission-race", move |_, _| {
                    body_called.store(true, Ordering::SeqCst);
                    async { Ok(()) }
                })
                .await
        }
    });

    authorization_entered.await;
    coordinator.request_exit_pending();
    gate.release.notify_one();

    assert!(matches!(
        operation.await.expect("operation task should join"),
        Err(OperationCoordinatorError::ExitPending)
    ));
    assert!(!body_called.load(Ordering::SeqCst));
    let idle = tokio::time::timeout(Duration::from_millis(100), coordinator.wait_until_idle())
        .await
        .expect("authorization permit should be released after rejection");
    drop(idle);
}

#[tokio::test]
async fn wait_until_idle_waits_for_active_body_and_retains_handoff_lease() {
    let coordinator = OperationCoordinator::default();
    let body_entered = Arc::new(Notify::new());
    let body_release = Arc::new(Notify::new());
    let entered = body_entered.notified();

    let operation = tokio::spawn({
        let coordinator = coordinator.clone();
        let body_entered = body_entered.clone();
        let body_release = body_release.clone();
        async move {
            coordinator
                .run_async(OperationKind::Flashing, "active", move |_, _| async move {
                    body_entered.notify_one();
                    body_release.notified().await;
                    Ok(())
                })
                .await
        }
    });
    entered.await;
    coordinator.request_exit_pending();

    let (lease_acquired_tx, mut lease_acquired_rx) = tokio::sync::oneshot::channel();
    let (lease_release_tx, lease_release_rx) = tokio::sync::oneshot::channel();
    let waiter = tokio::spawn({
        let coordinator = coordinator.clone();
        async move {
            let idle = coordinator.wait_until_idle().await;
            let _ = lease_acquired_tx.send(());
            let _ = lease_release_rx.await;
            drop(idle);
        }
    });

    assert!(matches!(
        lease_acquired_rx.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ));
    body_release.notify_one();
    operation
        .await
        .expect("operation task should join")
        .expect("active operation should finish naturally");
    tokio::time::timeout(Duration::from_millis(100), &mut lease_acquired_rx)
        .await
        .expect("idle waiter should receive the released permit")
        .expect("idle waiter should signal lease acquisition");

    assert!(matches!(
        coordinator
            .run_async(OperationKind::Flashing, "too-late", |_, _| async { Ok(()) })
            .await,
        Err(OperationCoordinatorError::ExitPending)
    ));
    let _ = lease_release_tx.send(());
    waiter.await.expect("idle waiter should join");
}

#[tokio::test]
async fn protected_exit_never_cancels_an_already_admitted_operation() {
    let coordinator = OperationCoordinator::default();
    let body_entered = Arc::new(Notify::new());
    let entered = body_entered.notified();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let cancellation_observed = Arc::new(AtomicBool::new(false));

    let operation = tokio::spawn({
        let coordinator = coordinator.clone();
        let body_entered = body_entered.clone();
        let cancellation_observed = cancellation_observed.clone();
        async move {
            coordinator
                .run_async(
                    OperationKind::Flashing,
                    "protected",
                    move |_, token| async move {
                        body_entered.notify_one();
                        tokio::select! {
                            _ = token.cancelled() => {
                                cancellation_observed.store(true, Ordering::SeqCst);
                            }
                            _ = release_rx => {}
                        }
                        Ok(())
                    },
                )
                .await
        }
    });

    entered.await;
    coordinator.request_exit_pending();
    assert!(!operation.is_finished());
    assert!(!cancellation_observed.load(Ordering::SeqCst));
    let _ = release_tx.send(());

    operation
        .await
        .expect("operation task should join")
        .expect("protected exit should allow natural completion");
    assert!(!cancellation_observed.load(Ordering::SeqCst));
}

#[tokio::test]
async fn admission_state_is_monotonic_and_never_reopens() {
    let coordinator = OperationCoordinator::default();
    let requesters = (0..16).map(|_| {
        let coordinator = coordinator.clone();
        tokio::spawn(async move { coordinator.request_exit_pending() })
    });

    for requester in requesters {
        let state = requester.await.expect("requester should join");
        assert_eq!(state, OperationAdmissionState::ExitPending);
    }

    let idle = coordinator.wait_until_idle().await;
    coordinator
        .begin_terminating(&idle)
        .expect("idle lease should authorize the terminal transition");
    assert_eq!(
        coordinator.request_exit_pending(),
        OperationAdmissionState::Terminating
    );
    assert_eq!(
        coordinator.admission_state(),
        OperationAdmissionState::Terminating
    );
    assert!(matches!(
        coordinator.try_acquire_idle(),
        Err(OperationCoordinatorError::Terminating)
    ));
}

#[tokio::test]
async fn dispose_keeps_idle_waiter_live_and_never_cancels_active_operation() {
    let coordinator = OperationCoordinator::default();
    let body_entered = Arc::new(Notify::new());
    let entered = body_entered.notified();
    let (body_release_tx, body_release_rx) = tokio::sync::oneshot::channel();
    let cancellation_observed = Arc::new(AtomicBool::new(false));
    let operation = tokio::spawn({
        let coordinator = coordinator.clone();
        let body_entered = body_entered.clone();
        let cancellation_observed = cancellation_observed.clone();
        async move {
            coordinator
                .run_async(
                    OperationKind::Flashing,
                    "dispose-active",
                    move |_, token| async move {
                        body_entered.notify_one();
                        tokio::select! {
                            _ = token.cancelled() => {
                                cancellation_observed.store(true, Ordering::SeqCst);
                            }
                            _ = body_release_rx => {}
                        }
                        Ok(())
                    },
                )
                .await
        }
    });
    entered.await;

    let (idle_acquired_tx, mut idle_acquired_rx) = tokio::sync::oneshot::channel();
    let waiter = tokio::spawn({
        let coordinator = coordinator.clone();
        async move {
            let idle = coordinator.wait_until_idle().await;
            let _ = idle_acquired_tx.send(());
            idle
        }
    });
    coordinator.dispose();

    assert!(!cancellation_observed.load(Ordering::SeqCst));
    assert!(matches!(
        idle_acquired_rx.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ));
    let _ = body_release_tx.send(());
    operation
        .await
        .expect("operation task should join")
        .expect("dispose should permit natural completion");
    idle_acquired_rx
        .await
        .expect("idle waiter should acquire after active completion");
    let idle = waiter.await.expect("idle waiter should join");

    assert!(!cancellation_observed.load(Ordering::SeqCst));
    assert!(matches!(
        coordinator
            .run_async(OperationKind::Flashing, "disposed", |_, _| async { Ok(()) })
            .await,
        Err(OperationCoordinatorError::Disposed)
    ));
    drop(idle);
}

impl OperationPermissionGate for BlockingPermissionGate {
    fn authorize(
        &self,
        _operation: OperationKind,
        _title: String,
    ) -> BoxFuture<'static, Result<OperationAuthorization, DomainError>> {
        let entered = self.entered.clone();
        let release = self.release.clone();
        async move {
            entered.notify_one();
            release.notified().await;
            Ok(OperationAuthorization::allow())
        }
        .boxed()
    }
}

#[tokio::test]
async fn pending_authorization_blocks_idle_lease_acquisition() {
    let gate = BlockingPermissionGate::default();
    let authorization_entered = gate.entered.notified();
    let coordinator =
        OperationCoordinator::new(None, Some(Arc::new(gate.clone())), None, None, None);

    let operation = tokio::spawn({
        let coordinator = coordinator.clone();
        async move {
            coordinator
                .run_async(
                    OperationKind::Flashing,
                    "authorization-pending",
                    |_, _| async { Ok(()) },
                )
                .await
        }
    });

    authorization_entered.await;

    assert!(matches!(
        coordinator.try_acquire_idle(),
        Err(OperationCoordinatorError::InProgress)
    ));

    gate.release.notify_one();
    operation
        .await
        .expect("operation task should join")
        .expect("operation should finish after authorization resumes");
}

#[tokio::test]
async fn idle_lease_blocks_operation_admission_until_dropped() {
    let coordinator = OperationCoordinator::default();
    let idle_lease = coordinator
        .try_acquire_idle()
        .expect("idle coordinator should grant a lease");

    let blocked = coordinator
        .run_async(
            OperationKind::Flashing,
            "blocked-by-idle-lease",
            |_, _| async { Ok(()) },
        )
        .await;
    assert!(matches!(
        blocked,
        Err(OperationCoordinatorError::InProgress)
    ));

    drop(idle_lease);

    coordinator
        .run_async(
            OperationKind::Flashing,
            "admitted-after-drop",
            |_, _| async { Ok(()) },
        )
        .await
        .expect("dropping the idle lease should release operation admission");
}

#[tokio::test]
async fn run_async_rejects_concurrent_operations_immediately() {
    let coordinator =
        OperationCoordinator::new(None, Some(PermissionGate::allow()), None, None, None);

    let first_started = Arc::new(AtomicBool::new(false));
    let first_started_inner = first_started.clone();
    let first = tokio::spawn({
        let coordinator = coordinator.clone();
        async move {
            coordinator
                .run_async(OperationKind::Flashing, "task-A", move |_ctx, _| {
                    let first_started_inner = first_started_inner.clone();
                    async move {
                        first_started_inner.store(true, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(80)).await;
                        Ok(())
                    }
                })
                .await
                .unwrap();
        }
    });

    while !first_started.load(Ordering::SeqCst) {
        tokio::task::yield_now().await;
    }

    let second_err = coordinator
        .run_async(OperationKind::Flashing, "task-B", |_ctx, _| async move {
            Ok(())
        })
        .await;

    assert!(matches!(
        second_err,
        Err(OperationCoordinatorError::InProgress)
    ));
    first.await.unwrap();
}

#[tokio::test]
async fn run_async_permission_deny_does_not_start_operation_or_usage_log() {
    let usage = Recorder::default();
    let logs = TestLogger::default();
    let coordinator = OperationCoordinator::new(
        None,
        Some(PermissionGate::deny("无权限")),
        Some(Arc::new(usage.clone())),
        Some(Arc::new(logs.clone())),
        None,
    );

    let denied = coordinator
        .run_async(OperationKind::Installing, "被拒绝任务", |_ctx, _| async {
            Ok(())
        })
        .await
        .expect_err("permission denied should be error");

    match denied {
        OperationCoordinatorError::Denied(message) => {
            assert!(message.contains("无权限"));
        }
        _ => panic!("unexpected result"),
    }

    let snapshot = coordinator.state().await;
    assert_eq!(snapshot.kind, OperationKind::Idle);
    assert!(usage.logs.lock().unwrap().is_empty());
    assert!(logs
        .entries()
        .iter()
        .any(|message| message.contains("许可")));
}

#[tokio::test]
async fn run_async_associates_operation_logs_with_the_running_operation_id() {
    let logger = OperationIdLogger::default();
    let coordinator = OperationCoordinator::new(
        None,
        Some(PermissionGate::allow()),
        None,
        Some(Arc::new(logger.clone())),
        None,
    );

    coordinator
        .run_async(
            OperationKind::Transferring,
            "下载设备文件",
            |ctx, _| async move {
                ctx.report_stage("正在传输设备文件");
                tokio::time::sleep(Duration::from_millis(10)).await;
                Ok(())
            },
        )
        .await
        .expect("operation should complete");

    let entries = logger.entries();
    assert!(entries
        .iter()
        .any(|(message, _)| message == "正在传输设备文件"));
    let operation_id = entries[0]
        .1
        .as_deref()
        .filter(|value| !value.is_empty())
        .expect("operation log should carry its operation id");
    assert!(entries
        .iter()
        .all(|(_, id)| id.as_deref() == Some(operation_id)));
}

#[tokio::test]
async fn run_async_maps_cancellation_to_domain_error() {
    let coordinator =
        OperationCoordinator::new(None, Some(PermissionGate::allow()), None, None, None);
    let cancel_done = Arc::new(AtomicBool::new(false));
    let cancel_done_inner = cancel_done.clone();

    let operation = {
        let cancel_done_inner = cancel_done_inner.clone();
        move |_ctx: OperationContext, token: CancellationToken| async move {
            token.cancelled().await;
            cancel_done_inner.store(true, Ordering::SeqCst);
            Err(DomainError::UserCancelled("用户取消".to_string()))
        }
    };

    let task_coordinator = coordinator.clone();
    let task = tokio::spawn(async move {
        task_coordinator
            .run_async(OperationKind::Transferring, "刷写", operation)
            .await
    });

    tokio::time::sleep(Duration::from_millis(10)).await;
    coordinator.cancel_current().await;

    let err = task.await.unwrap().unwrap_err();
    assert!(matches!(err, OperationCoordinatorError::Canceled));
    assert!(cancel_done.load(Ordering::SeqCst));
}

#[tokio::test]
async fn run_async_hides_sensitive_tool_diagnostics_from_callers_state_and_logs() {
    let logger = TestLogger::default();
    let coordinator = OperationCoordinator::new(
        None,
        Some(PermissionGate::allow()),
        None,
        Some(Arc::new(logger.clone())),
        None,
    );

    let error = coordinator
        .run_async(OperationKind::Flashing, "受控刷写", |_ctx, _| async move {
            Err(DomainError::ExternalTool(
                "C:\\Users\\private\\image.img https://api.github.com/x?token=secret token=secret"
                    .to_string(),
            ))
        })
        .await
        .expect_err("a tool failure should be returned as a safe category");

    let snapshot = coordinator.state().await;
    let messages = [
        error.to_string(),
        snapshot.stage,
        logger.entries().join("\n"),
    ];
    for message in messages {
        assert!(!message.contains("C:\\Users"));
        assert!(!message.contains("api.github.com"));
        assert!(!message.contains("token=secret"));
    }
}

#[tokio::test]
async fn run_async_throttles_progress_events_to_hundred_milliseconds() {
    let coordinator =
        OperationCoordinator::new(None, Some(PermissionGate::allow()), None, None, None);
    let mut state_changes = coordinator.subscribe_state();

    let heavy_task = tokio::spawn({
        let coordinator = coordinator.clone();
        async move {
            coordinator
                .run_async(
                    OperationKind::Mirroring,
                    "进度任务",
                    move |ctx, _| async move {
                        for index in 0..80u32 {
                            let progress = (index as f64) / 80.0;
                            ctx.report_progress(progress);
                            tokio::time::sleep(Duration::from_millis(2)).await;
                        }
                        Ok(())
                    },
                )
                .await
        }
    });

    let mut received = 0u32;
    while let Ok(Ok(snapshot)) =
        tokio::time::timeout(Duration::from_millis(700), state_changes.recv()).await
    {
        if snapshot.kind == OperationKind::Completed {
            received += 1;
            break;
        }
        received += 1;
    }

    let result = heavy_task.await.unwrap();
    assert!(result.is_ok());
    assert!(received <= 20);
}

#[tokio::test]
async fn monotonic_progress_rejects_a_late_lower_update_at_coordinator_application_time() {
    let coordinator =
        OperationCoordinator::new(None, Some(PermissionGate::allow()), None, None, None);
    let observed = Arc::new(std::sync::Mutex::new(None));
    let observed_for_run = observed.clone();
    let coordinator_for_run = coordinator.clone();

    coordinator
        .run_async(
            OperationKind::Flashing,
            "Safe Flash progress",
            move |context, _| async move {
                context.report_progress_monotonic(0.95);
                tokio::task::yield_now().await;
                context.report_progress_monotonic(0.20);
                tokio::time::sleep(Duration::from_millis(20)).await;
                *observed_for_run.lock().unwrap() =
                    Some(coordinator_for_run.state().await.progress);
                Ok(())
            },
        )
        .await
        .expect("operation should succeed");

    assert_eq!(*observed.lock().unwrap(), Some(Some(0.95)));
}

#[tokio::test]
async fn run_async_publishes_a_path_free_partition_task_snapshot() {
    let coordinator =
        OperationCoordinator::new(None, Some(PermissionGate::allow()), None, None, None);
    let mut state_changes = coordinator.subscribe_state();

    let task = tokio::spawn({
        let coordinator = coordinator.clone();
        async move {
            coordinator
                .run_async(
                    OperationKind::Flashing,
                    "分区写入",
                    |context, _| async move {
                        context
                            .report_partition_task("boot_a", PartitionTaskState::Running, 0.25)
                            .await;
                        tokio::time::sleep(Duration::from_millis(5)).await;
                        Ok(())
                    },
                )
                .await
        }
    });

    let mut partition_snapshot = None;
    while let Ok(Ok(snapshot)) =
        tokio::time::timeout(Duration::from_millis(200), state_changes.recv()).await
    {
        if snapshot.partition_task.is_some() {
            partition_snapshot = snapshot.partition_task;
            break;
        }
    }
    task.await
        .expect("operation task should finish")
        .expect("operation should succeed");

    let partition_snapshot = partition_snapshot.expect("partition progress should be published");
    assert_eq!(partition_snapshot.partition_name, "boot_a");
    assert_eq!(partition_snapshot.state, PartitionTaskState::Running);
    assert_eq!(partition_snapshot.overall_progress, 0.25);
}

#[tokio::test]
async fn partition_task_updates_preserve_every_row_in_the_latest_snapshot() {
    let coordinator =
        OperationCoordinator::new(None, Some(PermissionGate::allow()), None, None, None);
    let mut state_changes = coordinator.subscribe_state();

    let task = tokio::spawn({
        let coordinator = coordinator.clone();
        async move {
            coordinator
                .run_async(
                    OperationKind::Flashing,
                    "分区写入",
                    |context, _| async move {
                        context
                            .report_partition_task("boot_a", PartitionTaskState::Failed, 0.25)
                            .await;
                        context
                            .report_partition_task(
                                "vendor_boot_a",
                                PartitionTaskState::Canceled,
                                0.25,
                            )
                            .await;
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        Ok(())
                    },
                )
                .await
        }
    });

    let mut terminal_rows = None;
    while let Ok(Ok(snapshot)) =
        tokio::time::timeout(Duration::from_millis(200), state_changes.recv()).await
    {
        if snapshot.partition_tasks.len() == 2 {
            terminal_rows = Some(snapshot.partition_tasks);
            break;
        }
    }
    task.await
        .expect("operation task should finish")
        .expect("operation should succeed");

    assert_eq!(
        terminal_rows.expect("terminal rows should be published"),
        vec![
            nwflash_domain::PartitionTaskSnapshot {
                partition_name: "boot_a".to_string(),
                state: PartitionTaskState::Failed,
                overall_progress: 0.25,
            },
            nwflash_domain::PartitionTaskSnapshot {
                partition_name: "vendor_boot_a".to_string(),
                state: PartitionTaskState::Canceled,
                overall_progress: 0.25,
            },
        ]
    );
}

#[tokio::test]
async fn run_async_records_usage_status_for_each_outcome() {
    let usage = UsageEntryRecorder::default();
    let coordinator = OperationCoordinator::new(
        None,
        Some(PermissionGate::allow()),
        Some(Arc::new(usage.clone())),
        None,
        None,
    );

    coordinator
        .run_async(OperationKind::Flashing, "成功任务", |_ctx, _| async move {
            Ok(())
        })
        .await
        .unwrap();
    coordinator
        .run_async(OperationKind::Flashing, "失败任务", |_ctx, _| async move {
            Err(DomainError::Internal("失败".to_string()))
        })
        .await
        .unwrap_err();
    coordinator
        .run_async(OperationKind::Flashing, "取消任务", |_, token| async move {
            token.cancel();
            Err(DomainError::UserCancelled("用户取消".to_string()))
        })
        .await
        .unwrap_err();

    let statuses = usage.statuses();
    assert_eq!(statuses, vec!["success", "failed", "canceled"]);
    let event_ids = usage.event_ids();
    assert_eq!(event_ids.len(), 3);
    assert_ne!(event_ids[0], event_ids[1]);
    assert_ne!(event_ids[1], event_ids[2]);
}

#[test]
fn result_to_domain_error_maps_all_branches() {
    let canceled = result_to_domain_error(OperationCoordinatorError::Canceled);
    assert!(matches!(canceled, DomainError::UserCancelled(_)));
}
