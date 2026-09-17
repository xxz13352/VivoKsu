use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use nwflash_application::MirrorService;

fn temporary_directory(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be available")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("nwflash-mirror-{label}-{nonce}"));
    fs::create_dir_all(&path).expect("temporary directory should be created");
    path
}

fn fixture_tools(root: &Path) -> (PathBuf, PathBuf) {
    let scrcpy = root.join("scrcpy").join("scrcpy.exe");
    let adb = root.join("platform-tools").join("adb.exe");
    fs::create_dir_all(scrcpy.parent().expect("scrcpy parent should exist"))
        .expect("scrcpy directory should be created");
    fs::create_dir_all(adb.parent().expect("adb parent should exist"))
        .expect("adb directory should be created");
    fs::write(&scrcpy, b"scrcpy").expect("scrcpy fixture should be written");
    fs::write(&adb, b"adb").expect("adb fixture should be written");
    (scrcpy, adb)
}

#[test]
fn mirror_builds_scrcpy_with_the_platform_adb_environment_variable() {
    let root = temporary_directory("start");
    let (scrcpy, adb) = fixture_tools(&root);
    let service = MirrorService::new(&scrcpy, &adb);

    let command = service
        .build_start_command("RF8T123", true)
        .expect("ADB-connected device should build a mirror command");

    assert_eq!(command.program, scrcpy.to_string_lossy());
    assert_eq!(command.args, vec!["--serial", "RF8T123", "--stay-awake"]);
    assert_eq!(
        command.environment,
        vec![("ADB".to_string(), adb.to_string_lossy().into_owned())]
    );
    assert!(!command.args.iter().any(|argument| argument == "--adb-path"));
    fs::remove_dir_all(root).expect("temporary directory should be removed");
}

#[test]
fn mirror_does_not_build_a_command_when_scrcpy_is_missing() {
    let root = temporary_directory("missing");
    let adb = root.join("platform-tools").join("adb.exe");
    fs::create_dir_all(adb.parent().expect("adb parent should exist"))
        .expect("adb directory should be created");
    fs::write(&adb, b"adb").expect("adb fixture should be written");
    let service = MirrorService::new(root.join("scrcpy.exe"), &adb);

    let error = service
        .build_start_command("RF8T123", true)
        .expect_err("missing scrcpy must not start a mirror process");

    assert!(error.to_string().contains("scrcpy"));
    fs::remove_dir_all(root).expect("temporary directory should be removed");
}

#[test]
fn manual_stop_suppresses_a_following_automatic_reconcile_until_reenabled() {
    let root = temporary_directory("deliberate-stop");
    let (scrcpy, adb) = fixture_tools(&root);
    let mut service = MirrorService::new(&scrcpy, &adb);
    service.set_auto_mirror_enabled(true);

    assert!(service
        .reconcile_command("RF8T123", true)
        .expect("automatic reconcile should be valid")
        .is_some());

    service.stop();

    assert!(service
        .reconcile_command("RF8T123", true)
        .expect("automatic reconcile should remain valid")
        .is_none());

    service.set_auto_mirror_enabled(true);
    assert!(service
        .reconcile_command("RF8T123", true)
        .expect("re-enabled automatic mirror should be valid")
        .is_some());
    fs::remove_dir_all(root).expect("temporary directory should be removed");
}
