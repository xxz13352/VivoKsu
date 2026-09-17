use nwflash_domain::{
    build_quick_flash_plan, FastbootTarget, FlashImageInfo, QuickFlashOptions, QuickFlashPartition,
    QuickFlashRequest,
};

#[test]
fn build_plan_writes_single_partition_without_slot_mode() {
    let image = FlashImageInfo {
        path: "C:\\images\\boot.img".to_string(),
        size_bytes: 4,
    };
    let requests = vec![QuickFlashRequest {
        partition: QuickFlashPartition::Boot,
        image: image.clone(),
    }];
    let options = QuickFlashOptions {
        target: FastbootTarget::Fastboot,
        wait_for_device: true,
        flash_both_slots: false,
        switch_slot_after_flash: false,
        auto_reboot: true,
    };
    let plan = build_quick_flash_plan(&requests, &options, None, |_| false)
        .expect("plan should be created");

    assert_eq!(plan.requests.len(), 1);
    assert_eq!(plan.requests[0].partition_name, "boot");
    assert_eq!(plan.requests[0].image_path, image.path);
    assert!(plan.switch_to_slot.is_none());
}

#[test]
fn build_plan_flashes_each_partition_to_a_and_b() {
    let requests = vec![
        QuickFlashRequest {
            partition: QuickFlashPartition::Boot,
            image: FlashImageInfo {
                path: "C:\\images\\boot.bin".to_string(),
                size_bytes: 8,
            },
        },
        QuickFlashRequest {
            partition: QuickFlashPartition::InitBoot,
            image: FlashImageInfo {
                path: "C:\\images\\init_boot.bin".to_string(),
                size_bytes: 8,
            },
        },
    ];
    let options = QuickFlashOptions {
        target: FastbootTarget::Fastboot,
        wait_for_device: true,
        flash_both_slots: true,
        switch_slot_after_flash: false,
        auto_reboot: false,
    };

    let plan = build_quick_flash_plan(&requests, &options, Some("_a"), |partition| {
        matches!(partition, "boot" | "init_boot")
    })
    .expect("plan should be created");

    let actual: Vec<&str> = plan
        .requests
        .iter()
        .map(|item| item.partition_name.as_str())
        .collect();
    assert_eq!(
        actual,
        vec!["boot_a", "boot_b", "init_boot_a", "init_boot_b"]
    );
}

#[test]
fn build_plan_prevents_unsupported_dual_slot_partition_before_execution() {
    let requests = vec![
        QuickFlashRequest {
            partition: QuickFlashPartition::Boot,
            image: FlashImageInfo {
                path: "C:\\images\\boot.bin".to_string(),
                size_bytes: 8,
            },
        },
        QuickFlashRequest {
            partition: QuickFlashPartition::VendorBoot,
            image: FlashImageInfo {
                path: "C:\\images\\vendor_boot.bin".to_string(),
                size_bytes: 8,
            },
        },
    ];
    let options = QuickFlashOptions {
        target: FastbootTarget::Fastboot,
        wait_for_device: true,
        flash_both_slots: true,
        switch_slot_after_flash: false,
        auto_reboot: false,
    };

    let err = build_quick_flash_plan(&requests, &options, None, |partition| partition == "boot");
    assert!(err.is_err());
    assert!(err.unwrap_err().to_string().contains("不支持 A/B 双槽刷写"));
}

#[test]
fn build_plan_switches_to_other_slot_after_plan_when_requested() {
    let image = FlashImageInfo {
        path: "C:\\images\\boot.img".to_string(),
        size_bytes: 4,
    };
    let requests = vec![QuickFlashRequest {
        partition: QuickFlashPartition::Boot,
        image,
    }];
    let options = QuickFlashOptions {
        target: FastbootTarget::Fastboot,
        wait_for_device: true,
        flash_both_slots: true,
        switch_slot_after_flash: true,
        auto_reboot: true,
    };

    let plan = build_quick_flash_plan(&requests, &options, Some("b"), |_| true).unwrap();
    assert_eq!(plan.switch_to_slot.as_deref(), Some("a"));
}

#[test]
fn build_plan_parses_raw_getvar_current_slot_output() {
    let image = FlashImageInfo {
        path: "C:\\images\\boot.img".to_string(),
        size_bytes: 4,
    };
    let requests = vec![QuickFlashRequest {
        partition: QuickFlashPartition::Boot,
        image,
    }];
    let options = QuickFlashOptions {
        target: FastbootTarget::Fastboot,
        wait_for_device: true,
        flash_both_slots: true,
        switch_slot_after_flash: true,
        auto_reboot: true,
    };

    // `fastboot getvar current-slot` returns the `key: value` line plus a
    // trailing `finished. total time: …` summary, not a bare letter.
    let plan = build_quick_flash_plan(
        &requests,
        &options,
        Some("current-slot: a\nfinished. total time: 0.123s"),
        |_| true,
    )
    .expect("raw getvar output should be normalized to the active slot");

    assert_eq!(plan.switch_to_slot.as_deref(), Some("b"));
}
