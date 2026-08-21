use std::{
    fs::{self, File},
    io::Write,
    time::{SystemTime, UNIX_EPOCH},
};

use flate2::{write::GzEncoder, Compression};
use nwflash_application::{FirmwareExtractEntry, FirmwareExtractService};
use nwflash_infrastructure::FirmwareFormat;
use zip4::{write::SimpleFileOptions, ZipWriter};

#[test]
fn inspect_local_vivo_archive_projects_path_safe_partition_metadata() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be available")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("nwflash-application-firmware-{nonce}"));
    fs::create_dir_all(&root).expect("fixture directory should be created");
    let archive = root.join("vivo_ota.gz");
    write_gzip_tar(&archive, "release/images/boot.img", b"boot");

    let inspection = FirmwareExtractService::inspect_local(&archive)
        .expect("valid local VIVO archive should be inspected");

    assert_eq!(inspection.format, FirmwareFormat::VivoGzipTar);
    assert_eq!(inspection.entries.len(), 1);
    assert_eq!(inspection.entries[0].id, "0");
    assert_eq!(inspection.entries[0].name, "boot.img");
    assert_eq!(inspection.entries[0].size_bytes, 4);
    assert!(!inspection.entries[0].id.contains("release"));

    fs::remove_dir_all(root).expect("fixture directory should be removed");
}

#[test]
fn extract_local_vivo_archive_uses_only_selected_opaque_ids() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be available")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("nwflash-vivo-extract-{nonce}"));
    fs::create_dir_all(&root).expect("fixture directory should be created");
    let archive = root.join("vivo_ota.gz");
    write_gzip_tar(&archive, "release/images/boot.img", b"boot");
    let output = root.join("output");

    let images = FirmwareExtractService::extract_local(&archive, &["0".to_string()], &output)
        .expect("selected VIVO image should be extracted");

    assert_eq!(images.len(), 1);
    assert_eq!(images[0].size_bytes, 4);
    assert_eq!(
        fs::read(output.join("boot.img")).expect("image should be extracted"),
        b"boot"
    );

    fs::remove_dir_all(root).expect("fixture directory should be removed");
}

#[test]
fn extract_local_vivo_archive_propagates_cancellation() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be available")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("nwflash-vivo-cancel-{nonce}"));
    fs::create_dir_all(&root).expect("fixture directory should be created");
    let archive = root.join("vivo_ota.gz");
    write_gzip_tar(&archive, "release/images/boot.img", b"boot");
    let output = root.join("output");

    let error = FirmwareExtractService::extract_local_with_cancel(
        &archive,
        &["0".to_string()],
        &output,
        || true,
    )
    .expect_err("canceled extraction must fail");

    assert!(error.to_string().contains("取消"));
    assert!(!output.join("boot.img").exists());

    fs::remove_dir_all(root).expect("fixture directory should be removed");
}

#[test]
fn extract_payload_runs_the_controlled_tool_and_returns_verified_images() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be available")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("nwflash-payload-extract-{nonce}"));
    fs::create_dir_all(&root).expect("fixture directory should be created");
    let executable = root.join("payload_dumper.cmd");
    let output = root.join("output");
    fs::write(
        &executable,
        "@echo off\r\nset output=\r\nset partitions=\r\n:next\r\nif \"%~1\"==\"\" goto done\r\nif \"%~1\"==\"-i\" set partitions=%~2\r\nif \"%~1\"==\"-o\" set output=%~2\r\nshift\r\ngoto next\r\n:done\r\nfor %%p in (%partitions:,= %) do >\"%output%\\%%p.img\" echo payload\r\nexit /b 0\r\n",
    )
    .expect("payload tool script should be written");

    let images = FirmwareExtractService::extract_payload(
        &executable,
        "source.payload",
        &["boot".to_string()],
        &output,
        || false,
    )
    .expect("controlled payload tool should write a selected image");

    assert_eq!(images.len(), 1);
    assert_eq!(images[0].size_bytes, 9);
    assert!(output.join("boot.img").exists());

    fs::remove_dir_all(root).expect("fixture directory should be removed");
}

#[test]
fn extract_payload_rejects_duplicate_output_names_before_creating_the_user_output_directory() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be available")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("nwflash-payload-duplicates-{nonce}"));
    fs::create_dir_all(&root).expect("fixture directory should be created");
    let output = root.join("output");

    let error = FirmwareExtractService::extract_payload(
        &root.join("unused-payload_dumper.cmd"),
        "source.payload",
        &["boot".to_string(), "BOOT".to_string()],
        &output,
        || false,
    )
    .expect_err("case-insensitive duplicate payload names must be rejected before any write");

    assert!(matches!(
        error,
        nwflash_application::FirmwareExtractApplicationError::InvalidSelection
    ));
    assert!(!output.exists());
    fs::remove_dir_all(root).expect("fixture directory should be removed");
}

#[test]
fn extract_payload_reports_bytes_written_to_its_private_staging_directory() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be available")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("nwflash-payload-progress-{nonce}"));
    fs::create_dir_all(&root).expect("fixture directory should be created");
    let executable = root.join("payload_dumper.cmd");
    let output = root.join("output");
    fs::write(
        &executable,
        "@echo off\r\nset output=\r\n:next\r\nif \"%~1\"==\"\" goto done\r\nif \"%~1\"==\"-o\" set output=%~2\r\nshift\r\ngoto next\r\n:done\r\n>\"%output%\\boot.img\" echo payload\r\nping -n 3 127.0.0.1 >nul\r\nexit /b 0\r\n",
    )
    .expect("payload tool script should be written");
    let mut updates = Vec::new();

    FirmwareExtractService::extract_payload_with_progress(
        &executable,
        "source.payload",
        &["boot".to_string()],
        &output,
        || false,
        |current_partition, written_bytes| updates.push((current_partition, written_bytes)),
    )
    .expect("controlled payload tool should report its staged output");

    assert!(updates
        .iter()
        .any(|(partition, bytes)| partition.as_deref() == Some("boot") && *bytes > 0));
    fs::remove_dir_all(root).expect("fixture directory should be removed");
}

#[test]
fn extract_payload_with_metadata_reports_monotonic_progress_across_publication() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be available")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("nwflash-payload-phase-progress-{nonce}"));
    fs::create_dir_all(&root).expect("fixture directory should be created");
    let executable = root.join("payload_dumper.cmd");
    let output = root.join("output");
    fs::write(
        &executable,
        "@echo off\r\nset output=\r\n:next\r\nif \"%~1\"==\"\" goto done\r\nif \"%~1\"==\"-o\" set output=%~2\r\nshift\r\ngoto next\r\n:done\r\n>\"%output%\\boot.img\" echo payload\r\nping -n 3 127.0.0.1 >nul\r\nexit /b 0\r\n",
    )
    .expect("payload tool script should be written");
    let selected = [FirmwareExtractEntry {
        id: "0".to_string(),
        name: "boot".to_string(),
        size_bytes: 9,
    }];
    let mut updates = Vec::new();

    FirmwareExtractService::extract_payload_with_expected_sizes_and_progress(
        &executable,
        "source.payload",
        &selected,
        &output,
        || false,
        |_, bytes| updates.push(bytes),
    )
    .expect("metadata-verified payload should be published");

    assert!(updates.windows(2).all(|pair| pair[0] <= pair[1]));
    assert_eq!(updates.last(), Some(&9));
    fs::remove_dir_all(root).expect("fixture directory should be removed");
}

#[test]
fn extract_payload_does_not_accept_stale_user_output_when_the_tool_omits_a_selected_image() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be available")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("nwflash-payload-stale-output-{nonce}"));
    fs::create_dir_all(&root).expect("fixture directory should be created");
    let executable = root.join("payload_dumper.cmd");
    let output = root.join("output");
    fs::create_dir_all(&output).expect("user output directory should be created");
    fs::write(output.join("boot.img"), b"stale").expect("stale output should be written");
    fs::write(&executable, "@echo off\r\nexit /b 0\r\n")
        .expect("payload tool script should be written");

    let error = FirmwareExtractService::extract_payload(
        &executable,
        "source.payload",
        &["boot".to_string()],
        &output,
        || false,
    )
    .expect_err("a successful tool exit without a new staged image must fail");

    assert!(error.to_string().contains("未生成所选分区镜像"));
    assert_eq!(
        fs::read(output.join("boot.img")).expect("stale user output should remain untouched"),
        b"stale"
    );
    fs::remove_dir_all(root).expect("fixture directory should be removed");
}

#[test]
fn extract_payload_rejects_a_staged_partition_that_is_smaller_than_its_metadata_size() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be available")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("nwflash-payload-truncated-output-{nonce}"));
    fs::create_dir_all(&root).expect("fixture directory should be created");
    let executable = root.join("payload_dumper.cmd");
    let output = root.join("output");
    fs::write(
        &executable,
        "@echo off\r\nset output=\r\n:next\r\nif \"%~1\"==\"\" goto done\r\nif \"%~1\"==\"-o\" set output=%~2\r\nshift\r\ngoto next\r\n:done\r\n>\"%output%\\boot.img\" echo bad\r\nexit /b 0\r\n",
    )
    .expect("payload tool script should be written");
    let selected = [FirmwareExtractEntry {
        id: "0".to_string(),
        name: "boot".to_string(),
        size_bytes: 9,
    }];

    let error = FirmwareExtractService::extract_payload_with_expected_sizes_and_progress(
        &executable,
        "source.payload",
        &selected,
        &output,
        || false,
        |_, _| {},
    )
    .expect_err("truncated staged output must not be published");

    assert!(error.to_string().contains("大小"));
    assert!(!output.join("boot.img").exists());
    fs::remove_dir_all(root).expect("fixture directory should be removed");
}

#[test]
fn inspect_payload_runs_the_controlled_tool_and_projects_metadata_without_paths() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be available")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("nwflash-payload-inspect-{nonce}"));
    fs::create_dir_all(&root).expect("fixture directory should be created");
    let executable = root.join("payload_dumper.cmd");
    let metadata = root.join("metadata");
    fs::write(
        &executable,
        "@echo off\r\nset output=\r\n:next\r\nif \"%~1\"==\"\" goto done\r\nif \"%~1\"==\"-o\" set output=%~2\r\nshift\r\ngoto next\r\n:done\r\n>\"%output%\\metadata.json\" echo {\"partitions\":[{\"partition_name\":\"boot\",\"size_in_bytes\":4,\"compression_type\":\"none\"}]}\r\nexit /b 0\r\n",
    )
    .expect("payload tool script should be written");

    let inspection =
        FirmwareExtractService::inspect_payload(&executable, "source.payload", &metadata, || false)
            .expect("controlled payload tool should produce parseable metadata");

    assert_eq!(inspection.format, FirmwareFormat::Payload);
    assert_eq!(inspection.entries.len(), 1);
    assert_eq!(inspection.entries[0].id, "0");
    assert_eq!(inspection.entries[0].name, "boot");
    assert_eq!(inspection.entries[0].size_bytes, 4);

    fs::remove_dir_all(root).expect("fixture directory should be removed");
}

#[test]
fn inspect_payload_propagates_process_cancellation() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be available")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("nwflash-payload-cancel-{nonce}"));
    fs::create_dir_all(&root).expect("fixture directory should be created");
    let executable = root.join("payload_dumper.cmd");
    fs::write(&executable, "@echo off\r\nexit /b 0\r\n")
        .expect("payload tool script should be written");

    let error = FirmwareExtractService::inspect_payload(
        &executable,
        "source.payload",
        &root.join("metadata"),
        || true,
    )
    .expect_err("canceled payload metadata inspection must fail");

    assert!(matches!(
        error,
        nwflash_application::FirmwareExtractApplicationError::Canceled
    ));
    fs::remove_dir_all(root).expect("fixture directory should be removed");
}

#[test]
fn extract_payload_propagates_process_cancellation() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be available")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("nwflash-payload-extract-cancel-{nonce}"));
    fs::create_dir_all(&root).expect("fixture directory should be created");
    let executable = root.join("payload_dumper.cmd");
    fs::write(&executable, "@echo off\r\nexit /b 0\r\n")
        .expect("payload tool script should be written");

    let error = FirmwareExtractService::extract_payload(
        &executable,
        "source.payload",
        &["boot".to_string()],
        &root.join("output"),
        || true,
    )
    .expect_err("canceled payload extraction must fail");

    assert!(matches!(
        error,
        nwflash_application::FirmwareExtractApplicationError::Canceled
    ));
    fs::remove_dir_all(root).expect("fixture directory should be removed");
}

#[test]
fn inspect_local_image_directory_lists_sorted_nonempty_images_without_paths() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be available")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("nwflash-directory-firmware-{nonce}"));
    fs::create_dir_all(&root).expect("fixture directory should be created");
    fs::write(root.join("vendor_boot.img"), b"vendor").expect("vendor image should be written");
    fs::write(root.join("boot.img"), b"boot").expect("boot image should be written");
    fs::write(root.join("empty.img"), []).expect("empty image should be written");
    fs::write(root.join("notes.txt"), b"ignored").expect("note should be written");

    let inspection =
        FirmwareExtractService::inspect_local(&root).expect("image directory should be inspected");

    assert_eq!(inspection.format, FirmwareFormat::ImageDirectory);
    assert_eq!(
        inspection
            .entries
            .iter()
            .map(|entry| (entry.id.as_str(), entry.name.as_str(), entry.size_bytes))
            .collect::<Vec<_>>(),
        vec![("0", "boot.img", 4), ("1", "vendor_boot.img", 6)]
    );

    fs::remove_dir_all(root).expect("fixture directory should be removed");
}

#[test]
fn extract_local_directory_exports_only_the_selected_opaque_image_id() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be available")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("nwflash-directory-export-{nonce}"));
    let source = root.join("source");
    let output = root.join("output");
    fs::create_dir_all(&source).expect("source directory should be created");
    fs::write(source.join("vendor_boot.img"), b"vendor").expect("vendor image should be written");
    fs::write(source.join("boot.img"), b"boot").expect("boot image should be written");

    let images = FirmwareExtractService::extract_local(&source, &["0".to_string()], &output)
        .expect("selected directory image should be exported");

    assert_eq!(images.len(), 1);
    assert_eq!(images[0].size_bytes, 4);
    assert_eq!(
        fs::read(output.join("boot.img")).expect("boot should be exported"),
        b"boot"
    );
    assert!(!output.join("vendor_boot.img").exists());
    fs::remove_dir_all(root).expect("fixture directory should be removed");
}

#[test]
fn inspect_local_zip_projects_images_without_archive_entry_paths() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be available")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("nwflash-zip-firmware-{nonce}"));
    fs::create_dir_all(&root).expect("fixture directory should be created");
    let archive = root.join("ota.zip");
    {
        let file = File::create(&archive).expect("zip fixture should be created");
        let mut zip = ZipWriter::new(file);
        let options = SimpleFileOptions::default();
        zip.start_file("release/vendor_boot.img", options)
            .expect("vendor image should be added");
        zip.write_all(b"vendor")
            .expect("vendor image should be written");
        zip.start_file("release/boot.img", options)
            .expect("boot image should be added");
        zip.write_all(b"boot")
            .expect("boot image should be written");
        zip.finish().expect("zip fixture should be finalized");
    }

    let inspection =
        FirmwareExtractService::inspect_local(&archive).expect("zip package should be inspected");

    assert_eq!(inspection.format, FirmwareFormat::Zip);
    assert_eq!(
        inspection
            .entries
            .iter()
            .map(|entry| (entry.id.as_str(), entry.name.as_str(), entry.size_bytes))
            .collect::<Vec<_>>(),
        vec![("0", "boot.img", 0), ("1", "vendor_boot.img", 0)]
    );

    fs::remove_dir_all(root).expect("fixture directory should be removed");
}

#[test]
fn extract_local_zip_exports_only_the_selected_opaque_image_id() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be available")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("nwflash-zip-export-{nonce}"));
    fs::create_dir_all(&root).expect("fixture directory should be created");
    let archive = root.join("ota.zip");
    {
        let file = File::create(&archive).expect("zip fixture should be created");
        let mut zip = ZipWriter::new(file);
        let options = SimpleFileOptions::default();
        zip.start_file("images/vendor_boot.img", options)
            .expect("vendor image should be added");
        zip.write_all(b"vendor")
            .expect("vendor image should be written");
        zip.start_file("images/boot.img", options)
            .expect("boot image should be added");
        zip.write_all(b"boot")
            .expect("boot image should be written");
        zip.finish().expect("zip fixture should be finalized");
    }
    let output = root.join("output");

    let images = FirmwareExtractService::extract_local(&archive, &["0".to_string()], &output)
        .expect("selected ZIP image should be exported");

    assert_eq!(images.len(), 1);
    assert_eq!(images[0].size_bytes, 4);
    assert_eq!(
        fs::read(output.join("boot.img")).expect("boot should be exported"),
        b"boot"
    );
    assert!(!output.join("vendor_boot.img").exists());
    fs::remove_dir_all(root).expect("fixture directory should be removed");
}

#[test]
fn inspect_line_flash_package_projects_only_managed_images() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be available")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("nwflash-line-package-{nonce}"));
    fs::create_dir_all(&root).expect("fixture directory should be created");
    let archive = root.join("firmware.zip");
    {
        let file = File::create(&archive).expect("zip fixture should be created");
        let mut zip = ZipWriter::new(file);
        let options = SimpleFileOptions::default();
        for (entry, data) in [
            ("images/vendor_boot.img", b"vendor".as_slice()),
            ("images/super.img", b"super".as_slice()),
            ("images/boot.img", b"boot".as_slice()),
        ] {
            zip.start_file(entry, options)
                .expect("image should be added");
            zip.write_all(data).expect("image should be written");
        }
        zip.finish().expect("zip fixture should be finalized");
    }

    let inspection = FirmwareExtractService::inspect_line_flash_package(&archive)
        .expect("line-flash ZIP package should be inspected");

    assert_eq!(inspection.format, FirmwareFormat::Zip);
    assert_eq!(
        inspection
            .entries
            .iter()
            .map(|entry| (entry.id.as_str(), entry.name.as_str()))
            .collect::<Vec<_>>(),
        vec![("0", "boot.img"), ("1", "vendor_boot.img")]
    );

    fs::remove_dir_all(root).expect("fixture directory should be removed");
}

#[test]
fn extract_line_flash_package_resolves_only_a_managed_opaque_id() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be available")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("nwflash-line-extract-{nonce}"));
    fs::create_dir_all(&root).expect("fixture directory should be created");
    let archive = root.join("firmware.zip");
    {
        let file = File::create(&archive).expect("zip fixture should be created");
        let mut zip = ZipWriter::new(file);
        let options = SimpleFileOptions::default();
        zip.start_file("images/boot.img", options)
            .expect("boot image should be added");
        zip.write_all(b"boot")
            .expect("boot image should be written");
        zip.start_file("images/super.img", options)
            .expect("super image should be added");
        zip.write_all(b"super")
            .expect("super image should be written");
        zip.finish().expect("zip fixture should be finalized");
    }
    let staging = root.join("staging");

    let image = FirmwareExtractService::extract_line_flash_package(&archive, "0", &staging)
        .expect("managed boot entry should be extracted");

    assert_eq!(image.size_bytes, 4);
    assert_eq!(
        fs::read(&image.path).expect("staged boot image should exist"),
        b"boot"
    );

    fs::remove_dir_all(root).expect("fixture directory should be removed");
}

#[test]
fn extract_line_flash_package_propagates_cancellation_without_staging_an_image() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be available")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("nwflash-line-cancel-{nonce}"));
    fs::create_dir_all(&root).expect("fixture directory should be created");
    let archive = root.join("firmware.zip");
    {
        let file = File::create(&archive).expect("zip fixture should be created");
        let mut zip = ZipWriter::new(file);
        zip.start_file("images/boot.img", SimpleFileOptions::default())
            .expect("boot image should be added");
        zip.write_all(b"boot")
            .expect("boot image should be written");
        zip.finish().expect("zip fixture should be finalized");
    }
    let staging = root.join("staging");

    let error = FirmwareExtractService::extract_line_flash_package_with_cancel(
        &archive,
        "0",
        &staging,
        || true,
    )
    .expect_err("canceled line-flash extraction must fail");

    assert!(error.to_string().contains("取消"));
    assert!(
        !staging.exists()
            || fs::read_dir(&staging)
                .expect("staging directory should remain readable")
                .next()
                .is_none()
    );

    fs::remove_dir_all(root).expect("fixture directory should be removed");
}

fn write_gzip_tar(path: &std::path::Path, name: &str, contents: &[u8]) {
    let file = File::create(path).expect("gzip fixture should be created");
    let mut gzip = GzEncoder::new(file, Compression::default());
    let mut header = [0u8; 512];
    header[..name.len()].copy_from_slice(name.as_bytes());
    let size = format!("{:011o}\0", contents.len());
    header[124..136].copy_from_slice(size.as_bytes());
    header[156] = b'0';
    gzip.write_all(&header)
        .expect("tar header should be written");
    gzip.write_all(contents)
        .expect("tar content should be written");
    gzip.write_all(&vec![0; (512 - (contents.len() % 512)) % 512])
        .expect("tar padding should be written");
    gzip.write_all(&[0; 1024])
        .expect("tar terminator should be written");
    gzip.finish().expect("gzip fixture should be finalized");
}
