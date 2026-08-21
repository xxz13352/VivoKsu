use nwflash_application::{
    parse_adb_root_partition_table, parse_fastboot_partition_table, PartitionWorkspace,
};
use nwflash_domain::{DevicePartition, FlashImageInfo, PartitionSnapshot, PartitionTransportKind};

#[test]
fn fastboot_partition_table_parser_projects_sizes_slots_and_risk_flags() {
    let snapshot = parse_fastboot_partition_table(
        "FAST123",
        "(bootloader) current-slot: b\n(bootloader) partition-size:boot_a:0x04000000\n(bootloader) partition-size:super:0x200000000\n",
    )
    .expect("valid fastboot output should parse");

    assert_eq!(snapshot.serial, "FAST123");
    assert_eq!(snapshot.transport, PartitionTransportKind::Fastboot);
    assert_eq!(snapshot.active_slot, "b");
    assert_eq!(snapshot.partitions[0].name, "boot_a");
    assert_eq!(snapshot.partitions[0].size_bytes, Some(64 * 1024 * 1024));
    assert!(snapshot.partitions[1].is_high_risk);
}

#[test]
fn adb_root_partition_table_parser_keeps_mounted_high_risk_partitions() {
    let snapshot = parse_adb_root_partition_table(
        "ADB123",
        "_b",
        "boot_b|/dev/block/sda13|67108864|0\nsuper|/dev/block/sda70|8589934592|1\nboot_b|/dev/block/sda13|67108864|0\n",
    )
    .expect("valid ADB Root discovery output should parse");

    assert_eq!(snapshot.serial, "ADB123");
    assert_eq!(snapshot.transport, PartitionTransportKind::AdbRoot);
    assert_eq!(snapshot.active_slot, "b");
    assert_eq!(snapshot.partitions.len(), 2);
    let super_partition = snapshot
        .partitions
        .iter()
        .find(|partition| partition.name == "super")
        .expect("super should be retained");
    assert_eq!(super_partition.device_path, "/dev/block/sda70");
    assert!(super_partition.is_mounted);
    assert!(super_partition.is_high_risk);
}

#[test]
fn workspace_builds_an_erase_plan_only_from_the_last_explicit_snapshot() {
    let mut workspace = PartitionWorkspace::new();
    let snapshot = PartitionSnapshot {
        serial: "FAST123".to_string(),
        transport: PartitionTransportKind::Fastboot,
        active_slot: "a".to_string(),
        partitions: vec![DevicePartition {
            name: "super".to_string(),
            device_path: "super".to_string(),
            size_bytes: Some(8 * 1024 * 1024 * 1024),
            slot: String::new(),
            is_mounted: false,
            is_high_risk: true,
            can_backup: true,
        }],
    };
    workspace.apply_snapshot(snapshot.clone());

    assert_eq!(workspace.cached_snapshot(), Some(snapshot));

    let plan = workspace
        .build_erase_plan(&["super".to_string()])
        .expect("the known selected partition should build a plan");

    assert_eq!(plan.serial, "FAST123");
    assert_eq!(plan.tasks[0].partition_name, "super");
}

#[test]
fn workspace_selection_summary_keeps_mounted_and_high_risk_counts_from_the_snapshot() {
    let mut workspace = PartitionWorkspace::new();
    workspace.apply_snapshot(PartitionSnapshot {
        serial: "ADB123".to_string(),
        transport: PartitionTransportKind::AdbRoot,
        active_slot: "a".to_string(),
        partitions: vec![
            DevicePartition {
                name: "boot_a".to_string(),
                device_path: "/dev/block/sda12".to_string(),
                size_bytes: Some(64),
                slot: "a".to_string(),
                is_mounted: false,
                is_high_risk: false,
                can_backup: true,
            },
            DevicePartition {
                name: "super".to_string(),
                device_path: "/dev/block/sda70".to_string(),
                size_bytes: Some(8 * 1024 * 1024 * 1024),
                slot: String::new(),
                is_mounted: true,
                is_high_risk: true,
                can_backup: true,
            },
        ],
    });

    let summary = workspace
        .selection_summary(&["boot_a".to_string(), "super".to_string()])
        .expect("known selected partitions should produce a confirmation summary");

    assert_eq!(summary.task_count, 2);
    assert_eq!(summary.high_risk_count, 1);
    assert_eq!(summary.mounted_count, 1);
}

#[test]
fn workspace_maps_a_slotless_image_to_the_active_slot_before_building_a_write_plan() {
    let mut workspace = PartitionWorkspace::new();
    workspace.apply_snapshot(PartitionSnapshot {
        serial: "FAST123".to_string(),
        transport: PartitionTransportKind::Fastboot,
        active_slot: "b".to_string(),
        partitions: vec![
            DevicePartition {
                name: "boot_a".to_string(),
                device_path: "boot_a".to_string(),
                size_bytes: Some(64 * 1024 * 1024),
                slot: "a".to_string(),
                is_mounted: false,
                is_high_risk: false,
                can_backup: true,
            },
            DevicePartition {
                name: "boot_b".to_string(),
                device_path: "boot_b".to_string(),
                size_bytes: Some(64 * 1024 * 1024),
                slot: "b".to_string(),
                is_mounted: false,
                is_high_risk: false,
                can_backup: true,
            },
        ],
    });

    let mapped = workspace.map_images(&[FlashImageInfo {
        path: r"C:\firmware\boot.img".to_string(),
        size_bytes: 1024,
    }]);
    let plan = workspace
        .build_write_plan(&["boot_b".to_string()])
        .expect("the active-slot image mapping should create a write task");

    assert_eq!(mapped, vec!["boot_b"]);
    assert_eq!(plan.tasks.len(), 1);
    assert_eq!(plan.tasks[0].partition_name, "boot_b");
    assert_eq!(
        plan.tasks[0].image_path.as_deref(),
        Some(r"C:\firmware\boot.img")
    );
}

#[test]
fn workspace_rejects_backup_when_the_last_snapshot_uses_fastboot() {
    let mut workspace = PartitionWorkspace::new();
    workspace.apply_snapshot(PartitionSnapshot {
        serial: "FAST123".to_string(),
        transport: PartitionTransportKind::Fastboot,
        active_slot: "a".to_string(),
        partitions: vec![DevicePartition {
            name: "boot_a".to_string(),
            device_path: "boot_a".to_string(),
            size_bytes: Some(64),
            slot: "a".to_string(),
            is_mounted: false,
            is_high_risk: false,
            can_backup: true,
        }],
    });

    let error = workspace
        .build_backup_plan(&["boot_a".to_string()], r"C:\backups")
        .expect_err("Fastboot has no partition read-back transport");

    assert!(error.to_string().contains("Fastboot"));
}
