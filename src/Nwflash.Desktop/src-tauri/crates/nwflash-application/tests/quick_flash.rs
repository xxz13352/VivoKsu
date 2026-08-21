use std::{
    fs,
    time::{SystemTime, UNIX_EPOCH},
};

use nwflash_application::QuickFlashService;
use nwflash_domain::{
    PartitionExecutionPlan, PartitionOperationKind, PartitionTask, PartitionTransportKind,
};
use nwflash_windows::{device_transport::DeviceTransport, platform_tools::PlatformTools};

fn make_service() -> QuickFlashService {
    let tools = PlatformTools::new("adb.exe", "fastboot.exe");
    QuickFlashService::new(DeviceTransport::new(tools))
}

#[test]
fn inspect_image_accepts_an_existing_non_empty_bin_and_projects_its_metadata() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be available")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("nwflash-quick-flash-{nonce}.bin"));
    fs::write(&path, [0x56, 0x4B, 0x53, 0x55]).expect("image fixture should be written");

    let image = make_service()
        .inspect_image(&path)
        .expect("non-empty bin image should be accepted");

    assert_eq!(image.path, path.to_string_lossy());
    assert_eq!(image.size_bytes, 4);
    fs::remove_file(path).expect("image fixture should be removed");
}

#[test]
fn quick_flash_builds_fastboot_flash_commands() {
    let service = make_service();
    let plan = PartitionExecutionPlan {
        serial: "SN-001".to_string(),
        transport: PartitionTransportKind::Fastboot,
        operation: PartitionOperationKind::Write,
        tasks: vec![PartitionTask {
            partition_name: "boot".to_string(),
            device_path: "/dev/block/boot".to_string(),
            image_path: Some("C:\\tmp\\boot.img".to_string()),
            output_path: None,
            size_bytes: None,
        }],
    };

    let commands = service
        .build_commands(&plan)
        .expect("commands should be prepared");

    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].program, "fastboot.exe");
    assert_eq!(
        commands[0].args,
        vec![
            "-s".to_string(),
            "SN-001".to_string(),
            "flash".to_string(),
            "boot".to_string(),
            "C:\\tmp\\boot.img".to_string(),
        ]
    );
}

#[test]
fn quick_flash_retargets_a_prepared_plan_before_building_commands() {
    let service = make_service();
    let prepared_plan = PartitionExecutionPlan {
        serial: "SERIAL-A".to_string(),
        transport: PartitionTransportKind::Fastboot,
        operation: PartitionOperationKind::Write,
        tasks: vec![PartitionTask {
            partition_name: "boot".to_string(),
            device_path: "/dev/block/boot".to_string(),
            image_path: Some("C:\\tmp\\boot.img".to_string()),
            output_path: None,
            size_bytes: None,
        }],
    };

    let execution_plan = service
        .retarget_execution_plan(&prepared_plan, "SERIAL-B")
        .expect("the current transport serial should replace the prepared serial");
    let commands = service
        .build_commands(&execution_plan)
        .expect("retargeted commands should be prepared");

    assert_eq!(execution_plan.serial, "SERIAL-B");
    assert_eq!(prepared_plan.serial, "SERIAL-A");
    assert_eq!(commands[0].args[0..4], ["-s", "SERIAL-B", "flash", "boot"]);
}

#[test]
fn quick_flash_builds_staged_commands_for_adb_root_writes() {
    let service = make_service();
    let plan = PartitionExecutionPlan {
        serial: "ADB-001".to_string(),
        transport: PartitionTransportKind::AdbRoot,
        operation: PartitionOperationKind::Write,
        tasks: vec![PartitionTask {
            partition_name: "boot".to_string(),
            device_path: "/dev/block/sda12".to_string(),
            image_path: Some(r"C:\images\boot.img".to_string()),
            output_path: None,
            size_bytes: None,
        }],
    };

    let task_commands = service
        .build_task_commands(&plan)
        .expect("ADB Root write commands should be prepared");
    assert_eq!(task_commands.len(), 1);
    assert_eq!(task_commands[0].commands.len(), 2);
    let staging_path = task_commands[0]
        .staging_path
        .as_deref()
        .expect("ADB Root writes should expose a staging path");
    assert_eq!(task_commands[0].commands[0].args[2], "push");
    assert_eq!(task_commands[0].commands[0].args[3], r"C:\images\boot.img");
    assert_eq!(task_commands[0].commands[0].args[4], staging_path);
    assert_eq!(task_commands[0].commands[1].args[2], "shell");
    assert!(task_commands[0].commands[1]
        .args
        .iter()
        .any(|argument| argument.contains(staging_path)));
    assert!(!task_commands[0].commands[1]
        .args
        .iter()
        .any(|argument| argument.contains(r"C:\images\boot.img")));
}

#[test]
fn quick_flash_rejects_missing_serial() {
    let service = make_service();
    let plan = PartitionExecutionPlan {
        serial: String::new(),
        transport: PartitionTransportKind::Fastboot,
        operation: PartitionOperationKind::Write,
        tasks: vec![PartitionTask {
            partition_name: "boot".to_string(),
            device_path: "/dev/block/boot".to_string(),
            image_path: Some("C:\\tmp\\boot.img".to_string()),
            output_path: None,
            size_bytes: None,
        }],
    };

    let err = service
        .build_commands(&plan)
        .expect_err("serial should be required");
    assert!(err.to_string().contains("设备序列号不能为空"));
}

#[test]
fn quick_flash_rejects_automatic_transport() {
    let service = make_service();
    let plan = PartitionExecutionPlan {
        serial: "SN-001".to_string(),
        transport: PartitionTransportKind::Automatic,
        operation: PartitionOperationKind::Erase,
        tasks: vec![PartitionTask {
            partition_name: "userdata".to_string(),
            device_path: "/dev/block/userdata".to_string(),
            image_path: None,
            output_path: None,
            size_bytes: None,
        }],
    };

    let err = service
        .build_commands(&plan)
        .expect_err("automatic transport is invalid");
    assert!(err.to_string().contains("执行计划必须使用已解析的设备通道"));
}
