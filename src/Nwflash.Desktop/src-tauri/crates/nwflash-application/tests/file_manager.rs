use std::{
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use nwflash_application::{parse_remote_listing, FileManagerService};

fn temporary_directory(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be available")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("nwflash-file-manager-{label}-{nonce}"));
    fs::create_dir_all(&path).expect("temporary directory should be created");
    path
}

#[test]
fn file_manager_uses_adb_pull_and_push_argument_arrays_without_shell_text() {
    let root = temporary_directory("commands");
    let source = root.join("update.zip");
    let target = root.join("selected-name.zip");
    fs::write(&source, b"fixture").expect("source fixture should be written");
    let service = FileManagerService::with_platform_tools("adb.exe", "fastboot.exe");

    let pull = service
        .build_pull_command("RF8", "/sdcard/Download/update.zip", &target)
        .expect("pull command should build");
    let push = service
        .build_push_command("RF8", &source, "/sdcard/Download")
        .expect("push command should build");

    assert_eq!(
        pull.args,
        vec![
            "-s",
            "RF8",
            "pull",
            "/sdcard/Download/update.zip",
            target.to_string_lossy().as_ref(),
        ]
    );
    assert_eq!(
        push.args,
        vec![
            "-s",
            "RF8",
            "push",
            source.to_string_lossy().as_ref(),
            "/sdcard/Download/update.zip",
        ]
    );
    assert!(!pull
        .args
        .iter()
        .chain(&push.args)
        .any(|argument| argument == "shell"));
    fs::remove_dir_all(root).expect("temporary directory should be removed");
}

#[test]
fn file_manager_rejects_remote_path_traversal_before_constructing_adb_arguments() {
    let root = temporary_directory("traversal");
    let target = root.join("output.bin");
    let service = FileManagerService::with_platform_tools("adb.exe", "fastboot.exe");

    let error = service
        .build_pull_command("RF8", "/sdcard/Download/../secret", &target)
        .expect_err("remote traversal must be rejected");

    assert!(error.to_string().contains("设备路径"));
    fs::remove_dir_all(root).expect("temporary directory should be removed");
}

#[test]
fn file_manager_rejects_an_empty_device_serial_before_constructing_push_arguments() {
    let root = temporary_directory("empty-serial");
    let source = root.join("update.zip");
    fs::write(&source, b"fixture").expect("source fixture should be written");
    let service = FileManagerService::with_platform_tools("adb.exe", "fastboot.exe");

    let error = service
        .build_push_command("", &source, "/sdcard/Download")
        .expect_err("empty serial must be rejected");

    assert!(error.to_string().contains("设备串口不能为空"));
    fs::remove_dir_all(root).expect("temporary directory should be removed");
}

#[test]
fn file_manager_uses_fixed_quoted_templates_for_remote_listing_and_delete() {
    let service = FileManagerService::with_platform_tools("adb.exe", "fastboot.exe");

    let list = service
        .build_list_command("RF8", "/sdcard/O'Brien")
        .expect("remote list command should build");
    let delete = service
        .build_delete_command("RF8", "/sdcard/Download/old file.zip")
        .expect("remote delete command should build");

    assert_eq!(
        list.args,
        vec!["-s", "RF8", "shell", "ls -laL -- '/sdcard/O'\\''Brien/'"]
    );
    assert_eq!(
        delete.args,
        vec![
            "-s",
            "RF8",
            "shell",
            "rm -rf -- '/sdcard/Download/old file.zip'"
        ]
    );
}

#[test]
fn file_manager_rejects_deleting_the_remote_root_and_non_apk_installs() {
    let root = temporary_directory("install");
    let image = root.join("boot.img");
    fs::write(&image, b"fixture").expect("image fixture should be written");
    let service = FileManagerService::with_platform_tools("adb.exe", "fastboot.exe");

    let delete_error = service
        .build_delete_command("RF8", "/")
        .expect_err("remote root must never be deletable");
    let install_error = service
        .build_install_apk_command("RF8", &image)
        .expect_err("only APK files are installable");

    assert!(delete_error.to_string().contains("根目录"));
    assert!(install_error.to_string().contains("APK"));
    fs::remove_dir_all(root).expect("temporary directory should be removed");
}

#[test]
fn remote_listing_parses_entries_and_sorts_directories_before_files() {
    let entries = parse_remote_listing(
        "/sdcard/Download",
        "-rw-rw---- 1 u0_a123 media_rw 2048 2026-08-10 11:21 update.zip\n\
         drwxrws--- 2 u0_a123 media_rw 4096 2026-08-10 11:20 Camera\n\
         lrwxrwxrwx 1 root root 7 2026-08-10 11:20 Link Name -> target\n\
         total 12\n",
    );

    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].name, "Camera");
    assert!(entries[0].is_directory);
    assert_eq!(entries[0].full_path, "/sdcard/Download/Camera");
    assert_eq!(entries[1].name, "Link Name");
    assert_eq!(entries[1].full_path, "/sdcard/Download/Link Name");
    assert!(!entries[1].is_directory);
    assert_eq!(entries[2].name, "update.zip");
    assert_eq!(entries[2].size_bytes, 2048);
}
