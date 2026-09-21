use nwflash_domain::{
    compute_targets, is_slot_based_mode, other_slot, should_simulate_keep_root_partition,
    should_simulate_partition_flash, should_simulate_safe_flash_partition, SafeFlashSlotMode,
};

#[test]
fn compute_targets_uses_expected_slot_mapping() {
    let cases = [
        (
            "boot",
            SafeFlashSlotMode::CurrentSlot,
            Some("a"),
            true,
            vec!["boot"],
        ),
        (
            "boot",
            SafeFlashSlotMode::OtherSlot,
            Some("a"),
            true,
            vec!["boot_b"],
        ),
        (
            "boot",
            SafeFlashSlotMode::OtherSlot,
            Some("b"),
            true,
            vec!["boot_a"],
        ),
        (
            "boot",
            SafeFlashSlotMode::OtherSlot,
            None,
            true,
            vec!["boot"],
        ),
        (
            "boot",
            SafeFlashSlotMode::OtherSlot,
            Some("a"),
            false,
            vec!["boot"],
        ),
        (
            "boot",
            SafeFlashSlotMode::BothSlots,
            Some("a"),
            true,
            vec!["boot_a", "boot_b"],
        ),
        (
            "boot",
            SafeFlashSlotMode::BothSlots,
            Some("a"),
            false,
            vec!["boot"],
        ),
    ];

    for (partition, mode, current, has_slot, expected) in cases {
        assert_eq!(
            compute_targets(partition, mode, current, has_slot),
            expected
        );
    }
}

#[test]
fn other_slot_maps_only_a_and_b() {
    assert_eq!(other_slot(Some("a")), Some("b"));
    assert_eq!(other_slot(Some("b")), Some("a"));
    assert_eq!(other_slot(Some("_a")), Some("b"));
    assert_eq!(other_slot(Some("_b")), Some("a"));
    assert_eq!(other_slot(None), None);
    assert_eq!(other_slot(Some("")), None);
    assert_eq!(other_slot(Some("c")), None);
}

#[test]
fn is_slot_based_mode_is_true_for_non_current_modes() {
    assert!(!is_slot_based_mode(SafeFlashSlotMode::CurrentSlot));
    assert!(is_slot_based_mode(SafeFlashSlotMode::OtherSlot));
    assert!(is_slot_based_mode(SafeFlashSlotMode::BothSlots));
}

#[test]
fn safe_flash_partition_filters_preloader_and_lk() {
    assert!(nwflash_domain::should_skip_safe_flash_partition("lk"));
    assert!(nwflash_domain::should_skip_safe_flash_partition("LK_A"));
    assert!(nwflash_domain::should_skip_safe_flash_partition("lk_b"));
    assert!(nwflash_domain::should_skip_safe_flash_partition("lk2"));
    assert!(!nwflash_domain::should_skip_safe_flash_partition("lksec"));
    assert!(nwflash_domain::should_skip_safe_flash_partition(
        "preloader"
    ));
    assert!(nwflash_domain::should_skip_safe_flash_partition(
        "preloader_raw"
    ));
    assert!(!nwflash_domain::should_skip_safe_flash_partition("boot"));
}

/// 受保护分区判定：
/// - lk / lk_… / lk<数字> / 含 preloader：两种模式下都受保护（不写入设备）；
/// - system/product/vendor/odm/system_ext/odm_dlkm/system_dlkm/vendor_dlkm：
///   只在勾选“安全刷写”时受保护，未勾选时照常刷写。
/// 带槽位后缀的变体按基名判定，避免 system_a 漏网。
#[test]
fn protected_partitions_depend_on_the_safe_flash_flag() {
    const SYSTEM_PARTITIONS: [&str; 8] = [
        "system",
        "product",
        "vendor",
        "odm",
        "system_ext",
        "odm_dlkm",
        "system_dlkm",
        "vendor_dlkm",
    ];

    for name in ["lk", "LK", "lk_a", "lk_b", "lk2", "preloader", "Preloader_A"] {
        assert!(
            should_simulate_safe_flash_partition(name, false),
            "{name} 未勾选安全刷写时也必须受保护"
        );
        assert!(
            should_simulate_safe_flash_partition(name, true),
            "{name} 勾选安全刷写时同样受保护"
        );
    }

    for name in SYSTEM_PARTITIONS {
        assert!(
            !should_simulate_safe_flash_partition(name, false),
            "{name} 未勾选安全刷写时应正常刷写"
        );
        assert!(
            should_simulate_safe_flash_partition(name, true),
            "{name} 勾选安全刷写时应受保护"
        );
        for slot in ["_a", "_b"] {
            let slotted = format!("{name}{slot}");
            assert!(
                should_simulate_safe_flash_partition(&slotted, true),
                "{slotted} 勾选安全刷写时应受保护"
            );
            assert!(
                !should_simulate_safe_flash_partition(&slotted, false),
                "{slotted} 未勾选安全刷写时应正常刷写"
            );
        }
    }

    // 近似名不得误伤：精确匹配基名，不做子串匹配。
    for name in [
        "lksec",
        "boot",
        "init_boot",
        "vendor_boot",
        "userdata",
        "my_system",
        "systemui",
        "vendor_boot_a",
    ] {
        assert!(
            !should_simulate_safe_flash_partition(name, true),
            "{name} 不属于受保护分区"
        );
    }
}

/// 勾选「保留 ROOT」时 boot/init_boot/vendor_boot 也只做假刷写；
/// 未勾选时必须真的刷进去。带槽位后缀的变体同样要覆盖（否则当前槽的
/// boot 会被写掉）。
#[test]
fn keep_root_boot_partitions_are_simulated_only_when_selected() {
    for name in [
        "boot",
        "BOOT",
        "boot_a",
        "boot_b",
        "init_boot",
        "init_boot_a",
        "vendor_boot_b",
    ] {
        assert!(
            should_simulate_keep_root_partition(name),
            "{name} 在保留 ROOT 时必须只做假刷写"
        );
        assert!(
            should_simulate_partition_flash(name, false, true),
            "{name}：保留 ROOT + 关闭安全刷写时仍受保护"
        );
        assert!(
            !should_simulate_partition_flash(name, false, false),
            "{name}：未勾选保留 ROOT 时必须真的刷写"
        );
    }

    for name in ["userdata", "vbmeta", "bootloader", "my_boot", "boot_c"] {
        assert!(
            !should_simulate_keep_root_partition(name),
            "{name} 不属于保留 ROOT 保护的启动分区"
        );
    }

    // 两个开关的并集：任一命中即只做假刷写。
    assert!(should_simulate_partition_flash("system", true, false));
    assert!(!should_simulate_partition_flash("system", false, true));
    assert!(should_simulate_partition_flash("lk", false, false));
    assert!(should_simulate_partition_flash("boot", true, true));
    assert!(!should_simulate_partition_flash("userdata", false, false));
}
