use std::{
    fs::{self, File},
    io::Write,
    time::{SystemTime, UNIX_EPOCH},
};

use nwflash_domain::QuickFlashPartition;
use nwflash_infrastructure::{FirmwarePackageExtractionService, FirmwarePackageInspector};
use zip::{write::SimpleFileOptions, ZipWriter};

#[test]
fn inspect_lists_sorted_images_and_keeps_managed_archive_entry_paths() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be available")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("nwflash-firmware-package-{nonce}"));
    fs::create_dir_all(&root).expect("fixture directory should be created");
    let package_path = root.join("PD2307_A14.zip");

    {
        let file = File::create(&package_path).expect("zip fixture should be created");
        let mut archive = ZipWriter::new(file);
        let options = SimpleFileOptions::default();
        archive
            .start_file("META-INF/com/android/metadata", options)
            .expect("metadata entry should be created");
        archive
            .write_all(b"device=vivo")
            .expect("metadata should be written");
        archive
            .start_file("images/vendor_boot.img", options)
            .expect("vendor boot entry should be created");
        archive
            .write_all(b"vendor")
            .expect("vendor boot should be written");
        archive
            .start_file("images/boot.img", options)
            .expect("boot entry should be created");
        archive.write_all(b"boot").expect("boot should be written");
        archive
            .start_file("images/super.img", options)
            .expect("super entry should be created");
        archive
            .write_all(b"super")
            .expect("super should be written");
        archive.finish().expect("zip fixture should be finalized");
    }

    let inspection =
        FirmwarePackageInspector::inspect(&package_path).expect("valid zip should be inspected");

    assert_eq!(inspection.package_name, "PD2307_A14.zip");
    assert_eq!(inspection.entry_count, 4);
    assert_eq!(
        inspection.image_entries,
        vec![
            "images/boot.img".to_string(),
            "images/super.img".to_string(),
            "images/vendor_boot.img".to_string(),
        ]
    );
    assert_eq!(
        inspection.managed_image_entries(),
        vec![
            "images/boot.img".to_string(),
            "images/vendor_boot.img".to_string(),
        ]
    );

    fs::remove_dir_all(root).expect("fixture directory should be removed");
}

#[test]
fn payload_archive_detection_matches_only_a_payload_bin_member_name() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be available")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("nwflash-payload-archive-{nonce}"));
    fs::create_dir_all(&root).expect("fixture directory should be created");
    let package_path = root.join("ota.zip");
    {
        let file = File::create(&package_path).expect("zip fixture should be created");
        let mut archive = ZipWriter::new(file);
        let options = SimpleFileOptions::default();
        archive
            .start_file("payload.bin.bak", options)
            .expect("non-payload entry should be added");
        archive
            .write_all(b"ignored")
            .expect("entry should be written");
        archive
            .start_file("OTA/PAYLOAD.BIN", options)
            .expect("payload entry should be added");
        archive
            .write_all(b"payload")
            .expect("entry should be written");
        archive.finish().expect("zip fixture should be finalized");
    }

    assert!(
        FirmwarePackageInspector::contains_payload_bin(&package_path)
            .expect("valid ZIP should be checked for payload.bin")
    );

    fs::remove_dir_all(root).expect("fixture directory should be removed");
}

#[test]
fn extract_managed_image_copies_it_to_a_unique_staging_file() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be available")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("nwflash-firmware-extract-{nonce}"));
    fs::create_dir_all(&root).expect("fixture directory should be created");
    let package_path = root.join("firmware.zip");

    {
        let file = File::create(&package_path).expect("zip fixture should be created");
        let mut archive = ZipWriter::new(file);
        let options = SimpleFileOptions::default();
        archive
            .start_file("images/init_boot.img", options)
            .expect("init boot entry should be created");
        archive
            .write_all(b"init-boot-payload")
            .expect("init boot should be written");
        archive
            .start_file("images/super.img", options)
            .expect("super entry should be created");
        archive
            .write_all(b"ignored")
            .expect("super should be written");
        archive.finish().expect("zip fixture should be finalized");
    }

    let inspection =
        FirmwarePackageInspector::inspect(&package_path).expect("valid zip should be inspected");
    let output_root = root.join("stage");
    let result = FirmwarePackageExtractionService::extract(
        &inspection,
        "images/init_boot.img",
        &output_root,
    )
    .expect("managed image should be extracted");

    assert_eq!(result.partition, QuickFlashPartition::InitBoot);
    assert_eq!(result.image.size_bytes, 17);
    assert!(result
        .image
        .path
        .starts_with(output_root.to_string_lossy().as_ref()));
    assert_eq!(
        fs::read(&result.image.path).expect("staged image should exist"),
        b"init-boot-payload"
    );

    fs::remove_dir_all(root).expect("fixture directory should be removed");
}

#[test]
fn export_inspected_zip_image_writes_only_the_selected_file_name_to_the_output_directory() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be available")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("nwflash-zip-export-{nonce}"));
    fs::create_dir_all(&root).expect("fixture directory should be created");
    let package_path = root.join("firmware.zip");
    {
        let file = File::create(&package_path).expect("zip fixture should be created");
        let mut archive = ZipWriter::new(file);
        let options = SimpleFileOptions::default();
        archive
            .start_file("nested/super.img", options)
            .expect("super entry should be added");
        archive
            .write_all(b"super")
            .expect("super should be written");
        archive.finish().expect("zip fixture should be finalized");
    }
    let inspection =
        FirmwarePackageInspector::inspect(&package_path).expect("zip should be inspected");
    let output = root.join("output");

    let image = FirmwarePackageExtractionService::export_image_to_directory_with_cancel(
        &inspection,
        "nested/super.img",
        &output,
        || false,
    )
    .expect("inspected ZIP image should be exported");

    assert_eq!(image.size_bytes, 5);
    assert_eq!(
        fs::read(output.join("super.img")).expect("image should be exported"),
        b"super"
    );
    assert!(!output.join("nested").exists());
    fs::remove_dir_all(root).expect("fixture directory should be removed");
}

#[test]
fn canceled_managed_image_extraction_removes_its_staging_output() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be available")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("nwflash-firmware-cancel-{nonce}"));
    fs::create_dir_all(&root).expect("fixture directory should be created");
    let package_path = root.join("firmware.zip");
    {
        let file = File::create(&package_path).expect("zip fixture should be created");
        let mut archive = ZipWriter::new(file);
        let options = SimpleFileOptions::default();
        archive
            .start_file("images/boot.img", options)
            .expect("boot entry should be created");
        archive.write_all(b"boot").expect("boot should be written");
        archive.finish().expect("zip fixture should be finalized");
    }
    let inspection =
        FirmwarePackageInspector::inspect(&package_path).expect("zip should be inspected");
    let staging = root.join("staging");

    let error = FirmwarePackageExtractionService::extract_with_cancel(
        &inspection,
        "images/boot.img",
        &staging,
        || true,
    )
    .expect_err("canceled extraction must fail");

    assert!(error.to_string().contains("取消"));
    assert!(
        !staging.exists()
            || fs::read_dir(&staging)
                .expect("staging should be readable")
                .next()
                .is_none()
    );
    fs::remove_dir_all(root).expect("fixture directory should be removed");
}
