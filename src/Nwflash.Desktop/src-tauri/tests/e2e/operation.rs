use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};

use futures::{future::BoxFuture, FutureExt};
use nwflash_application::{
    OperationAuthorization, OperationCoordinator, OperationCoordinatorError,
    OperationPermissionGate,
};
use nwflash_domain::{DomainError, OperationKind};

#[derive(Default)]
struct RecoveringPermissionGate {
    attempts: AtomicUsize,
}

impl OperationPermissionGate for RecoveringPermissionGate {
    fn authorize(
        &self,
        _operation: OperationKind,
        _title: String,
    ) -> BoxFuture<'static, Result<OperationAuthorization, DomainError>> {
        let attempt = self.attempts.fetch_add(1, Ordering::SeqCst);
        async move {
            if attempt == 0 {
                Err(DomainError::ExternalTool(
                    "Cloudflare authorization unavailable".to_string(),
                ))
            } else {
                Ok(OperationAuthorization::allow())
            }
        }
        .boxed()
    }
}

#[tokio::test]
async fn authorization_fault_does_not_run_the_device_closure_or_poison_the_coordinator() {
    let gate = Arc::new(RecoveringPermissionGate::default());
    let coordinator = OperationCoordinator::new(None, Some(gate), None, None, None);
    let closure_ran = Arc::new(AtomicBool::new(false));
    let closure_ran_after_fault = Arc::clone(&closure_ran);

    let failure = coordinator
        .run_async(
            OperationKind::Flashing,
            "faulted authorization",
            move |_, _| {
                closure_ran_after_fault.store(true, Ordering::SeqCst);
                async { Ok(()) }
            },
        )
        .await
        .expect_err("authorization transport failure should stop the operation");

    assert!(matches!(failure, OperationCoordinatorError::Failed(_)));
    assert!(!closure_ran.load(Ordering::SeqCst));
    assert!(!coordinator.is_busy());
    coordinator
        .run_async(OperationKind::Installing, "recovery", |_, _| async {
            Ok(())
        })
        .await
        .expect("a later authorized operation should run");
}

#[tokio::test]
async fn cancellation_finalizes_the_operation_and_releases_the_single_operation_gate() {
    let coordinator = OperationCoordinator::default();
    let running = tokio::spawn({
        let coordinator = coordinator.clone();
        async move {
            coordinator
                .run_async(
                    OperationKind::Flashing,
                    "cancelled flash",
                    |_, token| async move {
                        token.cancelled().await;
                        Err(DomainError::UserCancelled(
                            "fixture cancellation".to_string(),
                        ))
                    },
                )
                .await
        }
    });

    while !coordinator.is_busy() {
        tokio::task::yield_now().await;
    }
    coordinator.cancel_current().await;
    let result = running.await.expect("operation task should join");

    assert!(matches!(result, Err(OperationCoordinatorError::Canceled)));
    assert!(!coordinator.is_busy());
    coordinator
        .run_async(
            OperationKind::Discovering,
            "post-cancel detection",
            |_, _| async { Ok(()) },
        )
        .await
        .expect("cancellation must release the operation gate");
}

#[tokio::test]
async fn injected_device_failure_finalizes_and_allows_a_safe_retry() {
    let coordinator = OperationCoordinator::default();
    let result = coordinator
        .run_async(OperationKind::Discovering, "adb detection", |_, _| async {
            Err(DomainError::ExternalTool(
                "adb exited with code 1".to_string(),
            ))
        })
        .await;

    assert!(matches!(
        result,
        Err(OperationCoordinatorError::Failed(message))
            if message == "外部工具执行失败，请检查设备连接和所需组件后重试。"
    ));
    assert!(!coordinator.is_busy());
    coordinator
        .run_async(OperationKind::Discovering, "retry", |_, _| async { Ok(()) })
        .await
        .expect("failure finalization must release the operation gate");
}
