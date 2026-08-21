use nwflash_application::FileTransferService;
use nwflash_windows::{device_transport::DeviceTransport, platform_tools::PlatformTools};

fn make_service() -> FileTransferService {
    let tools = PlatformTools::new("adb.exe", "fastboot.exe");
    FileTransferService::new(DeviceTransport::new(tools))
}

#[test]
fn file_transfer_builds_pull_command() {
    let service = make_service();
    let command = service
        .build_pull_command("SN-002", "/dev/block/boot", "C:\\tmp\\boot.bin")
        .expect("pull command should build");

    assert_eq!(command.program, "adb.exe");
    assert_eq!(
        command.args,
        vec![
            "-s".to_string(),
            "SN-002".to_string(),
            "exec-out".to_string(),
            "su".to_string(),
            "--no-pty".to_string(),
            "-c".to_string(),
            "dd".to_string(),
            "if=/dev/block/boot".to_string(),
            "bs=4M".to_string(),
            "2>/dev/null".to_string(),
        ]
    );
    assert!(!command
        .args
        .iter()
        .any(|argument| argument.contains("boot.bin")));
}

#[test]
fn file_transfer_rejects_invalid_device_path() {
    let service = make_service();
    let err = service
        .build_pull_command("SN-002", "bad-path", "C:\\tmp\\boot.bin")
        .expect_err("bad path rejected");
    assert!(err.to_string().contains("设备路径"));
}

#[test]
fn file_transfer_rejects_empty_serial() {
    let service = make_service();
    let err = service
        .build_push_command("", "C:\\tmp\\boot.bin", "/dev/block/boot")
        .expect_err("serial required");
    assert!(err.to_string().contains("设备串口不能为空"));
}

#[test]
fn file_transfer_rejects_direct_adb_root_binary_stdin_writes() {
    let service = make_service();
    let err = service
        .build_push_command("SN-002", "C:\\tmp\\boot.bin", "/dev/block/boot")
        .expect_err("direct ADB Root stdin writes must be rejected");

    assert!(err.to_string().contains("暂存上传流程"));
}
