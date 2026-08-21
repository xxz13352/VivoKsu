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

    let error = start_plan(runtime.clone(), coordinator.clone(), plan)
        .await
        .expect_err("invalid scrcpy executable should fail at the real spawn boundary");

    assert_eq!(
        error,
        "内部错误: 外部工具执行失败，请检查设备连接和所需组件后重试。"
    );
    assert!(!runtime.status().is_mirroring);
    assert!(!coordinator.is_busy());
    coordinator
        .run_async(OperationKind::Mirroring, "投屏失败后重试", |_, _| async {
            Ok(())
        })
        .await
        .expect("spawn failure finalization must release the operation gate");
    fs::remove_dir_all(root).expect("temporary directory should be removed");
}
