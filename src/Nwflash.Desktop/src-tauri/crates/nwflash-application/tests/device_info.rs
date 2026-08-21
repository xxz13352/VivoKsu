use nwflash_application::{
    apply_fastboot_device_details, parse_adb_battery_level, parse_adb_device_details,
};
use nwflash_domain::DeviceDetailsSnapshot;

#[test]
fn parse_adb_device_details_reads_wpf_equivalent_getprop_fields() {
    let details = parse_adb_device_details(
        "RF8",
        "[ro.product.brand]: [vivo]\n[ro.product.model]: [V2318A]\n[ro.product.device]: [PD2307]\n[ro.build.version.release]: [15]\n[ro.build.display.id]: [OriginOS 5]\n",
    );

    assert_eq!(details.brand, "vivo");
    assert_eq!(details.model, "V2318A");
    assert_eq!(details.codename, "PD2307");
    assert_eq!(details.serial, "RF8");
    assert_eq!(details.android_version, "15");
    assert_eq!(details.firmware_version, "OriginOS 5");
}

#[test]
fn parse_adb_device_details_uses_unavailable_for_missing_or_malformed_properties() {
    let details = parse_adb_device_details("RF8", "[ro.product.brand]: [vivo]\ninvalid\n");

    assert_eq!(details.brand, "vivo");
    assert_eq!(details.model, "Not available");
    assert_eq!(details.android_version, "Not available");
}

#[test]
fn fastboot_details_preserve_known_adb_fields_and_apply_slot_and_bootloader_values() {
    let mut details = DeviceDetailsSnapshot::empty();
    details.serial = "FAST-1".to_string();
    details.model = "V2318A".to_string();
    details.android_version = "15".to_string();

    let result = apply_fastboot_device_details(details, "b", "yes", "PD2307");

    assert_eq!(result.model, "V2318A");
    assert_eq!(result.android_version, "15");
    assert_eq!(result.active_slot, "b");
    assert_eq!(result.bootloader_state, "unlocked");
}

#[test]
fn fastboot_details_fill_unavailable_model_from_product_and_normalize_locked_state() {
    let result =
        apply_fastboot_device_details(DeviceDetailsSnapshot::empty(), "_a", "no", "PD2307");

    assert_eq!(result.model, "PD2307");
    assert_eq!(result.codename, "PD2307");
    assert_eq!(result.active_slot, "a");
    assert_eq!(result.bootloader_state, "locked");
}

#[test]
fn parse_adb_battery_level_accepts_only_a_valid_percentage() {
    assert_eq!(
        parse_adb_battery_level("AC powered: false\nlevel: 78\nstatus: 3\n"),
        "78%"
    );
    assert_eq!(parse_adb_battery_level("level: 101\n"), "--");
    assert_eq!(parse_adb_battery_level("level: unknown\n"), "--");
    assert_eq!(parse_adb_battery_level("status: 3\n"), "--");
}
