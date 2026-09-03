use std::collections::HashMap;
use std::env::temp_dir;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use nwflash_domain::{
    format_partition_size, is_high_risk_partition, parse_fastboot_rs_output, DeviceConnectionState,
    PartitionExecutionPlanBuilder, PartitionTask, PartitionTransportKind,
};

#[test]
fn high_risk_partitions_should_be_flagged() {
    let critical = [
        "abl",
        "frp",
        "gpt",
        "lk",
        "metadata",
        "modemst",
        "modemst1",
        "modemst_a",
        "partition",
        "persist",
        "preloader",
        "super",
        "userdata",
        "vbmeta",
        "xbl",
        "xbl_1",
    ];
    for partition in critical {
        assert!(
            is_high_risk_partition(partition),
            "{partition} should be high risk"
        );
    }
}

#[test]
fn ordinary_partitions_should_not_be_flagged() {
    let normal = [
        "boot",
        "init_boot",
        "vendor_boot",
        "system",
        "cache",
        "recovery",
    ];
    for partition in normal {
        assert!(
            !is_high_risk_partition(partition),
            "{partition} should not be high risk"
        );
    }
}

#[test]
fn format_partition_size_matches_fastboot_rules() {
    assert_eq!(format_partition_size(1024 * 1024 * 64), "64 MB");
    assert_eq!(format_partition_size(1024 * 8), "8 KB");
    assert_eq!(format_partition_size(4096), "4 KB");
    assert_eq!(format_partition_size(512), "512 B");
    assert_eq!(format_partition_size(1024 * 1024 * 1024), "1 GB");
}

#[test]
fn build_write_keeps_the_selected_partition_and_any_img_filename() {
    let partition = create_partition("boot_a", "boot_a", Some(64 * 1024 * 1024));
    let mut image_paths = HashMap::new();
    let path = create_temp_file(&[0x00; 4]);
    image_paths.insert(partition.name.clone(), path.display().to_string());

    let plan = PartitionExecutionPlanBuilder
        .build_write(
            "FAST123",
            PartitionTransportKind::Fastboot,
            &[partition],
            &image_paths,
        )
        .expect("build_write should succeed");

    assert_eq!(plan.serial, "FAST123");
    assert_eq!(plan.transport, PartitionTransportKind::Fastboot);
    assert_eq!(
        plan.operation,
        nwflash_domain::PartitionOperationKind::Write
    );
    assert_eq!(plan.tasks.len(), 1);
    assert_eq!(
        plan.tasks[0],
        PartitionTask {
            partition_name: "boot_a".to_string(),
            device_path: "boot_a".to_string(),
            image_path: Some(path.display().to_string()),
            output_path: None,
            size_bytes: Some(64 * 1024 * 1024),
        }
    );
}

#[test]
fn build_write_rejects_missing_image_for_adb_root() {
    let partition = create_partition("boot_a", "boot_a", Some(64 * 1024 * 1024));
    let mut image_paths = HashMap::new();
    image_paths.insert(
        partition.name.clone(),
        temp_dir().join("missing-image.img").display().to_string(),
    );

    let err = PartitionExecutionPlanBuilder.build_write(
        "ADB123",
        PartitionTransportKind::AdbRoot,
        &[partition],
        &image_paths,
    );
    assert!(err.is_err());
    assert!(err.unwrap_err().to_string().contains("不存在"));
}

#[test]
fn build_write_rejects_empty_image_for_adb_root() {
    let partition = create_partition("boot_a", "boot_a", Some(64 * 1024 * 1024));
    let mut image_paths = HashMap::new();
    let path = create_temp_file(&[]);
    image_paths.insert(partition.name.clone(), path.display().to_string());

    let err = PartitionExecutionPlanBuilder.build_write(
        "ADB123",
        PartitionTransportKind::AdbRoot,
        &[partition],
        &image_paths,
    );
    assert!(err.is_err());
    assert!(err.unwrap_err().to_string().contains("为空"));
}

#[test]
fn build_write_rejects_image_larger_than_partition() {
    let partition = create_partition("boot_a", "boot_a", Some(4));
    let mut image_paths = HashMap::new();
    let path = create_temp_file(&[1, 2, 3, 4, 5]);
    image_paths.insert(partition.name.clone(), path.display().to_string());

    let err = PartitionExecutionPlanBuilder.build_write(
        "ADB123",
        PartitionTransportKind::AdbRoot,
        &[partition],
        &image_paths,
    );
    assert!(err.is_err());
    assert!(err.unwrap_err().to_string().contains("大于分区"));
}

#[test]
fn build_write_rejects_android_sparse_image_for_adb_root() {
    let partition = create_partition("boot_a", "boot_a", Some(64 * 1024 * 1024));
    let mut image_paths = HashMap::new();
    let path = create_temp_file(&[0x3A, 0xFF, 0x26, 0xED]);
    image_paths.insert(partition.name.clone(), path.display().to_string());

    let err = PartitionExecutionPlanBuilder.build_write(
        "ADB123",
        PartitionTransportKind::AdbRoot,
        &[partition],
        &image_paths,
    );
    assert!(err.is_err());
    assert!(err.unwrap_err().to_string().contains("sparse"));
}

#[test]
fn build_backup_assigns_img_output_under_directory() {
    let partition = create_partition("vendor_boot_b", "/dev/block/sda22", Some(96 * 1024 * 1024));
    let plan = PartitionExecutionPlanBuilder
        .build_backup(
            "ADB123",
            PartitionTransportKind::AdbRoot,
            &[partition],
            "D:\\backups",
        )
        .expect("build_backup should succeed");

    assert_eq!(plan.tasks.len(), 1);
    assert_eq!(
        plan.tasks[0].output_path.as_deref(),
        Some("D:\\backups\\vendor_boot_b.img")
    );
}

#[test]
fn build_erase_keeps_mounted_and_high_risk_partitions_in_plan() {
    let partition = create_partition("super", "/dev/block/sda70", Some(8 * 1024 * 1024 * 1024));
    let plan = PartitionExecutionPlanBuilder
        .build_erase("ADB123", PartitionTransportKind::AdbRoot, &[partition])
        .expect("build_erase should succeed");

    assert_eq!(plan.tasks.len(), 1);
    assert_eq!(plan.tasks[0].partition_name, "super");
    assert_eq!(plan.tasks[0].device_path, "/dev/block/sda70");
    assert!(plan.tasks[0].image_path.is_none());
}

#[test]
fn parse_fastboot_outputs_are_mapped_consistently() {
    let device = parse_fastboot_rs_output("1A2B3C4D\tdevice\n");
    assert_eq!(device.connection_state, DeviceConnectionState::AdbConnected);
    assert_eq!(device.serial, "1A2B3C4D");
    assert_eq!(device.connection_label, "ADB 已连接");

    let device = parse_fastboot_rs_output("FAST123\tfastboot (fastbootd)\n");
    assert_eq!(
        device.connection_state,
        DeviceConnectionState::FastbootConnected
    );
    assert_eq!(device.serial, "FAST123");
    assert_eq!(device.connection_label, "Fastbootd 已连接");

    let device = parse_fastboot_rs_output("");
    assert_eq!(device.connection_state, DeviceConnectionState::Disconnected);

    let device = parse_fastboot_rs_output("1A2B3C4D\toffline\n");
    assert_eq!(device.connection_state, DeviceConnectionState::Disconnected);
    assert_eq!(device.serial, "1A2B3C4D");

    let device = parse_fastboot_rs_output("1A2B3C4D\tno permissions (user in plugdev group)\n");
    assert_eq!(device.connection_state, DeviceConnectionState::Unauthorized);
    assert_eq!(device.serial, "1A2B3C4D");

    let device = parse_fastboot_rs_output("1A2B3C4D\tsome-unknown-state\n");
    assert_eq!(device.connection_state, DeviceConnectionState::Error);
    assert_eq!(device.serial, "1A2B3C4D");
}

/// `platform_tools::adb_devices_command` runs `adb devices -l`, whose real output
/// is space-padded and carries extra `key:value` columns after the state.  The
/// banner line is present in both `adb devices` and `adb devices -l`.
#[test]
fn parse_real_adb_devices_dash_l_output() {
    let device = parse_fastboot_rs_output(
        "List of devices attached\n\
         1A2B3C4D               device product:PD2307 model:V2307A device:PD2307 transport_id:1\n\n",
    );
    assert_eq!(device.connection_state, DeviceConnectionState::AdbConnected);
    assert_eq!(device.serial, "1A2B3C4D");
    assert_eq!(device.connection_label, "ADB 已连接");

    let device = parse_fastboot_rs_output(
        "List of devices attached\n1A2B3C4D               unauthorized usb:1-2\n",
    );
    assert_eq!(device.connection_state, DeviceConnectionState::Unauthorized);
    assert_eq!(device.serial, "1A2B3C4D");

    let device = parse_fastboot_rs_output(
        "List of devices attached\n1A2B3C4D               offline usb:1-2\n",
    );
    assert_eq!(device.connection_state, DeviceConnectionState::Disconnected);
    assert_eq!(device.serial, "1A2B3C4D");
}

/// The banner alone must not be mistaken for a connected device, and adb's daemon
/// chatter must not be parsed as a serial.
#[test]
fn parse_adb_devices_ignores_banner_and_daemon_chatter() {
    let device = parse_fastboot_rs_output("List of devices attached\n\n");
    assert_eq!(device.connection_state, DeviceConnectionState::Disconnected);

    let device = parse_fastboot_rs_output(
        "* daemon not running; starting now at tcp:5037\n\
         * daemon started successfully\n\
         List of devices attached\n\
         1A2B3C4D               device transport_id:1\n",
    );
    assert_eq!(device.connection_state, DeviceConnectionState::AdbConnected);
    assert_eq!(device.serial, "1A2B3C4D");
}

/// `fastboot devices` (no `-l`) stays TAB separated; `fastboot devices -l` pads
/// with spaces and appends a `usb:` column.  Both must map to Fastboot.
#[test]
fn parse_real_fastboot_devices_output() {
    let device = parse_fastboot_rs_output("FAST123\tfastboot\n");
    assert_eq!(
        device.connection_state,
        DeviceConnectionState::FastbootConnected
    );
    assert_eq!(device.serial, "FAST123");

    let device = parse_fastboot_rs_output("FAST123               fastboot usb:1-2\n");
    assert_eq!(
        device.connection_state,
        DeviceConnectionState::FastbootConnected
    );
    assert_eq!(device.serial, "FAST123");
}

/// Two real `-l` rows must still be reported as "multiple devices" rather than
/// silently picking the first one.
#[test]
fn parse_adb_devices_dash_l_detects_multiple_devices() {
    let device = parse_fastboot_rs_output(
        "List of devices attached\n\
         1A2B3C4D               device transport_id:1\n\
         5E6F7G8H               device transport_id:2\n",
    );
    assert_eq!(
        device.connection_state,
        DeviceConnectionState::MultipleDevices
    );
}

#[test]
fn build_backup_rejects_path_syntax_in_partition_names() {
    let partition = create_partition("..\\..\\Users\\Public\\evil", "/dev/block/sda1", None);
    let err = PartitionExecutionPlanBuilder
        .build_backup(
            "ADB123",
            PartitionTransportKind::AdbRoot,
            &[partition],
            "D:\\backups",
        )
        .expect_err("path syntax in a partition name must be rejected");
    assert!(err.to_string().contains("分区名包含非法字符"));
}

#[test]
fn build_write_rejects_a_selected_partition_without_an_image() {
    let selected = [
        create_partition("boot_a", "boot_a", None),
        create_partition("vendor_boot_a", "vendor_boot_a", None),
    ];
    let mut image_paths = HashMap::new();
    let path = create_temp_file(&[0x00; 4]);
    image_paths.insert("boot_a".to_string(), path.display().to_string());

    let err = PartitionExecutionPlanBuilder
        .build_write(
            "FAST123",
            PartitionTransportKind::Fastboot,
            &selected,
            &image_paths,
        )
        .expect_err("a selected partition without an image must fail the plan");
    assert!(err.to_string().contains("vendor_boot_a 未指定镜像文件"));
}

fn create_partition(
    name: &str,
    path: &str,
    size_bytes: Option<i64>,
) -> nwflash_domain::DevicePartition {
    nwflash_domain::DevicePartition {
        name: name.to_string(),
        device_path: path.to_string(),
        size_bytes,
        slot: "a".to_string(),
        is_mounted: false,
        is_high_risk: is_high_risk_partition(name),
        can_backup: false,
    }
}

fn create_temp_file(contents: &[u8]) -> PathBuf {
    let mut file_path = temp_dir();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time")
        .as_nanos();
    file_path.push(format!("nwflash-domain-{nanos}-test.img"));
    let mut file = File::create(&file_path).expect("tmp file");
    file.write_all(contents).expect("write tmp");
    file_path
}
