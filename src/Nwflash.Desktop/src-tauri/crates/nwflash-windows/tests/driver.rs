use std::{
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use nwflash_windows::{detect_drivers, DriverDetectionPaths};

fn temporary_directory(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be available")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("nwflash-{label}-{nonce}"));
    fs::create_dir_all(&path).expect("temporary directory should be created");
    path
}

#[test]
fn detect_drivers_requires_the_wpf_equivalent_markers_for_all_three_transports() {
    let root = temporary_directory("driver-detection");
    let driver_store = root.join("DriverStore");
    fs::create_dir_all(driver_store.join("android_winusb.inf_amd64"))
        .expect("adb marker should be created");
    fs::create_dir_all(driver_store.join("android_usb.inf_amd64"))
        .expect("fastboot marker should be created");
    let mediatek = driver_store.join("cdc-acm.inf_amd64");
    fs::create_dir_all(&mediatek).expect("mediatek marker should be created");
    fs::write(mediatek.join("cdc-acm.inf"), "Provider=MediaTek Inc.")
        .expect("mediatek inf should be written");

    let status = detect_drivers(&DriverDetectionPaths::new(vec![driver_store], Vec::new()));

    assert!(status.adb_installed);
    assert!(status.fastboot_installed);
    assert!(status.mediatek_installed);
    assert!(status.all_installed());
    fs::remove_dir_all(root).expect("temporary directory should be removed");
}

#[test]
fn detect_drivers_rejects_a_non_mediatek_cdc_acm_inf() {
    let root = temporary_directory("driver-non-mediatek");
    let driver_store = root.join("DriverStore");
    let serial = driver_store.join("cdc-acm.inf_amd64");
    fs::create_dir_all(&serial).expect("serial marker should be created");
    fs::write(serial.join("cdc-acm.inf"), "Provider=CH340").expect("serial inf should be written");

    let status = detect_drivers(&DriverDetectionPaths::new(vec![driver_store], Vec::new()));

    assert!(!status.mediatek_installed);
    assert!(!status.all_installed());
    fs::remove_dir_all(root).expect("temporary directory should be removed");
}
