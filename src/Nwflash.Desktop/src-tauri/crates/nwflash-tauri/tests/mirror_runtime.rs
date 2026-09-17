use std::{
    fs,
    time::{SystemTime, UNIX_EPOCH},
};

use nwflash_application::{MirrorService, OperationCoordinator};
use nwflash_domain::OperationKind;
use nwflash_tauri::{start_plan, MirrorRuntime};

#[tokio::test]
async fn spawn_failure_finalizes_the_mirror_operation_and_releases_the_coordinator() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be available")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("nwflash-tauri-mirror-failure-{nonce}"));
    let scrcpy = root.join("scrcpy.exe");
    let adb = root.join("adb.exe");
    fs::create_dir_all(&root).expect("temporary directory should be created");
    fs::write(&scrcpy, b"not a Windows executable")
        .expect("failing scrcpy fixture should be written");
    fs::write(&adb, b"adb fixture").expect("ADB fixture should be written");
    let plan = MirrorService::new(&scrcpy, &adb)
        .build_start_command("RF8T123", true)
        .expect("existing tool fixtures should build a controlled plan");
    let runtime = MirrorRuntime::new();
    let coordinator = OperationCoordinator::default();

    let error = start_plan(
        runtime.clone(),
        coordinator.clone(),
        // 已构建计划的立即就绪供给工厂（该测试只关心真实 spawn 边界的
        // 失败）。启动失败的具体原因直接透传给调用方（审计 B32），不再
        // 是通用「内部错误」文案。
        move |_| Box::pin(async move { Ok(plan) }),
    )
    .await
    .expect_err("invalid scrcpy executable should fail at the real spawn boundary");

    assert!(
        error.contains("启动 ADB 投屏失败"),
        "spawn failure must surface the concrete spawn error, got: {error}"
    );
    assert!(!runtime.status().is_mirroring);
    assert!(!coordinator.is_busy());
    coordinator
        .run_shared_async(OperationKind::Mirroring, "投屏失败后重试", |_, _| async {
            Ok(())
        })
        .await
        .expect("spawn failure finalization must release the operation gate");
    fs::remove_dir_all(root).expect("temporary directory should be removed");
}
