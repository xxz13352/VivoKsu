use nwflash_domain::{compute_targets, is_slot_based_mode, other_slot, SafeFlashSlotMode};

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
