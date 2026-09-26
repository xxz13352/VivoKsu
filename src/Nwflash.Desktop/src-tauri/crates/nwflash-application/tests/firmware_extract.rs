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

// ---------- payload（CrAU）测试 ----------
//
// 这些测试构造**真实的合成 payload**，不再模拟外部工具：内建实现直接在进程内
// 解析，所以测试也应当喂给它真正的 CrAU 字节，而不是一个假装会写文件的 .cmd。

fn varint(mut value: u64, out: &mut Vec<u8>) {
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if value == 0 {
            break;
        }
    }
}

fn tag(field: u32, wire: u32, out: &mut Vec<u8>) {
    varint(u64::from((field << 3) | wire), out);
}

fn length_delimited(field: u32, body: &[u8], out: &mut Vec<u8>) {
    tag(field, 2, out);
    varint(body.len() as u64, out);
    out.extend_from_slice(body);
}

/// 构造一个含指定分区的 CrAU payload（全部用 raw operation，单 extent）。
fn build_payload(block_size: u32, partitions: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut data = Vec::new();
    let mut partitions_blob = Vec::new();

    for (name, content) in partitions {
        let data_offset = data.len() as u64;
        data.extend_from_slice(content);

        let mut op = Vec::new();
        tag(1, 0, &mut op);
        varint(0, &mut op); // 0 = REPLACE
        tag(2, 0, &mut op);
        varint(data_offset, &mut op);
        tag(3, 0, &mut op);
        varint(content.len() as u64, &mut op);
        let mut extent = Vec::new();
        tag(1, 0, &mut extent);
        varint(0, &mut extent);
        tag(2, 0, &mut extent);
        varint(
            (content.len() as u64).div_ceil(u64::from(block_size)),
            &mut extent,
        );
        length_delimited(6, &extent, &mut op);

        let mut info = Vec::new();
        tag(1, 0, &mut info);
        varint(content.len() as u64, &mut info);

        let mut partition = Vec::new();
        length_delimited(1, name.as_bytes(), &mut partition);
        length_delimited(7, &info, &mut partition);
        length_delimited(8, &op, &mut partition);
        length_delimited(13, &partition, &mut partitions_blob);
    }

    let mut manifest = Vec::new();
    tag(3, 0, &mut manifest);
    varint(u64::from(block_size), &mut manifest);
    manifest.extend_from_slice(&partitions_blob);

    let mut payload = Vec::new();
    payload.extend_from_slice(b"CrAU");
    payload.extend_from_slice(&2u64.to_be_bytes());
    payload.extend_from_slice(&(manifest.len() as u64).to_be_bytes());
    payload.extend_from_slice(&0u32.to_be_bytes());
    payload.extend_from_slice(&manifest);
    payload.extend_from_slice(&data);
    payload
}

fn temp_root(label: &str) -> std::path::PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be available")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("nwflash-{label}-{nonce}"));
    fs::create_dir_all(&root).expect("fixture directory should be created");
    root
}

#[test]
fn extract_payload_writes_the_selected_partition_from_a_real_payload() {
    let root = temp_root("payload-extract");
    let payload = root.join("ota.bin");
    fs::write(
        &payload,
        build_payload(4096, &[("boot", b"boot-image".to_vec())]),
    )
    .expect("写入合成 payload");
    let output = root.join("output");

    let images = FirmwareExtractService::extract_payload(
        &root.join("unused.exe"),
        payload.to_string_lossy().as_ref(),
        &["boot".to_string()],
        &output,
        || false,
    )
    .expect("内建解析器应当提取出镜像");

    assert_eq!(images.len(), 1);
    assert_eq!(images[0].size_bytes, 10);
    assert_eq!(
        fs::read(output.join("boot.img")).expect("读取产物"),
        b"boot-image"
    );
    fs::remove_dir_all(root).expect("清理");
}

#[test]
fn extract_payload_rejects_duplicate_output_names_before_creating_the_user_output_directory() {
    let root = temp_root("payload-duplicates");
    let output = root.join("output");

    let error = FirmwareExtractService::extract_payload(
        &root.join("unused.exe"),
        "source.payload",
        &["boot".to_string(), "BOOT".to_string()],
        &output,
        || false,
    )
    .expect_err("重复分区名必须被拒绝");

    assert!(matches!(
        error,
        nwflash_application::FirmwareExtractApplicationError::InvalidSelection
    ));
    assert!(!output.exists(), "拒绝时不应创建输出目录");
    fs::remove_dir_all(root).expect("清理");
}

#[test]
fn extract_payload_reports_monotonic_progress_across_partitions() {
    let root = temp_root("payload-progress");
    let payload = root.join("ota.bin");
    // 单次读缓冲是 256 KiB，用 1 MiB 的分区才能观察到多次回调。
    let big = vec![0x5Au8; 1024 * 1024];
    fs::write(
        &payload,
        build_payload(4096, &[("system", big), ("boot", b"boot".to_vec())]),
    )
    .expect("写入合成 payload");
    let output = root.join("output");

    let mut samples: Vec<u64> = Vec::new();
    let images = FirmwareExtractService::extract_payload_with_progress(
        &root.join("unused.exe"),
        payload.to_string_lossy().as_ref(),
        &["system".to_string(), "boot".to_string()],
        &output,
        || false,
        |_, bytes| samples.push(bytes),
    )
    .expect("提取应当成功");

    assert_eq!(images.len(), 2);
    assert!(
        samples.windows(2).all(|w| w[0] <= w[1]),
        "进度必须单调不减: {samples:?}"
    );
    let total: u64 = images.iter().map(|i| i.size_bytes as u64).sum();
    assert_eq!(samples.last().copied(), Some(total), "终值等于总字节数");
    fs::remove_dir_all(root).expect("清理");
}

#[test]
fn extract_payload_does_not_publish_anything_when_a_partition_is_missing() {
    let root = temp_root("payload-missing");
    let payload = root.join("ota.bin");
    fs::write(&payload, build_payload(4096, &[("boot", b"boot".to_vec())]))
        .expect("写入合成 payload");
    let output = root.join("output");

    let error = FirmwareExtractService::extract_payload(
        &root.join("unused.exe"),
        payload.to_string_lossy().as_ref(),
        &["boot".to_string(), "nosuch".to_string()],
        &output,
        || false,
    )
    .expect_err("不存在的分区必须失败");

    assert!(error.to_string().contains("nosuch"));
    assert!(
        !output.join("boot.img").exists(),
        "失败时不得留下任何已发布的镜像"
    );
    fs::remove_dir_all(root).expect("清理");
}

#[test]
fn extract_payload_reports_a_stale_size_mismatch_between_inspect_and_extract() {
    let root = temp_root("payload-stale-size");
    let payload = root.join("ota.bin");
    fs::write(&payload, build_payload(4096, &[("boot", b"boot".to_vec())]))
        .expect("写入合成 payload");
    let output = root.join("output");

    // 调用方持有的尺寸来自先前的 inspect；若与 manifest 不一致，说明两次读到的
    // 固件不是同一份（例如 URL 背后换了内容），必须失败而不是继续。
    let stale = vec![FirmwareExtractEntry {
        id: "0".to_string(),
        name: "boot".to_string(),
        size_bytes: 999,
    }];
    let error = FirmwareExtractService::extract_payload_with_expected_sizes_and_progress(
        &root.join("unused.exe"),
        payload.to_string_lossy().as_ref(),
        &stale,
        &output,
        || false,
        |_, _| {},
    )
    .expect_err("尺寸不一致必须失败");

    assert!(error.to_string().contains("尺寸"), "实际: {error}");
    fs::remove_dir_all(root).expect("清理");
}

#[test]
fn inspect_payload_projects_partition_metadata_without_file_paths() {
    let root = temp_root("payload-inspect");
    let payload = root.join("ota.bin");
    fs::write(
        &payload,
        build_payload(
            4096,
            &[("system", vec![7u8; 8192]), ("boot", vec![1u8; 4096])],
        ),
    )
    .expect("写入合成 payload");

    let inspection = FirmwareExtractService::inspect_payload(
        &root.join("unused.exe"),
        payload.to_string_lossy().as_ref(),
        &root.join("metadata"),
        || false,
    )
    .expect("应当读出分区清单");

    assert_eq!(inspection.format, FirmwareFormat::Payload);
    assert_eq!(
        inspection
            .entries
            .iter()
            .map(|entry| (entry.id.as_str(), entry.name.as_str(), entry.size_bytes))
            .collect::<Vec<_>>(),
        vec![("0", "system", 8192), ("1", "boot", 4096)]
    );
    fs::remove_dir_all(root).expect("清理");
}

#[test]
fn inspect_payload_propagates_cancellation() {
    let root = temp_root("payload-inspect-cancel");
    let payload = root.join("ota.bin");
    fs::write(&payload, build_payload(4096, &[("boot", vec![1u8; 4096])]))
        .expect("写入合成 payload");

    let error = FirmwareExtractService::inspect_payload(
        &root.join("unused.exe"),
        payload.to_string_lossy().as_ref(),
        &root.join("metadata"),
        || true,
    )
    .expect_err("取消必须失败");

    assert!(matches!(
        error,
        nwflash_application::FirmwareExtractApplicationError::Canceled
    ));
    fs::remove_dir_all(root).expect("清理");
}

#[test]
fn extract_payload_propagates_cancellation() {
    let root = temp_root("payload-extract-cancel");
    let payload = root.join("ota.bin");
    fs::write(&payload, build_payload(4096, &[("boot", vec![1u8; 4096])]))
        .expect("写入合成 payload");
    let output = root.join("output");

    let error = FirmwareExtractService::extract_payload(
        &root.join("unused.exe"),
        payload.to_string_lossy().as_ref(),
        &["boot".to_string()],
        &output,
        || true,
    )
    .expect_err("取消必须失败");

    assert!(error.to_string().contains("取消"));
    assert!(!output.join("boot.img").exists());
    fs::remove_dir_all(root).expect("清理");
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
fn inspect_local_zip_and_directory_only_expose_managed_partitions() {
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
        // 非受控镜像：绝对不能出现在提取列表里，更不能被导出。
        zip.start_file("release/super.img", options)
            .expect("super image should be added");
        zip.write_all(b"super")
            .expect("super image should be written");
        zip.start_file("release/vbmeta.img", options)
            .expect("vbmeta image should be added");
        zip.write_all(b"vbmeta")
            .expect("vbmeta image should be written");
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

    // 用全量列表时代的索引（2 = super.img、3 = vbmeta.img 在全量中曾有效）
    // 重放选择，必须被受控列表的越界校验拒绝。
    let error =
        FirmwareExtractService::extract_local(&archive, &["2".to_string()], &root.join("out"))
            .expect_err("non-managed zip selection must be rejected");
    assert!(matches!(
        error,
        nwflash_application::FirmwareExtractApplicationError::InvalidSelection
    ));

    fs::remove_dir_all(root).expect("fixture directory should be removed");
}

#[test]
fn inspect_local_directory_only_exposes_managed_partitions_and_rejects_leaked_indexes() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be available")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("nwflash-directory-firmware-{nonce}"));
    fs::create_dir_all(&root).expect("fixture directory should be created");
    fs::write(root.join("boot.img"), b"boot").expect("boot image should be written");
    fs::write(root.join("super.img"), b"super").expect("super image should be written");
    fs::write(root.join("vbmeta.img"), b"vbmeta").expect("vbmeta image should be written");

    let inspection =
        FirmwareExtractService::inspect_local(&root).expect("image directory should be inspected");

    assert_eq!(inspection.format, FirmwareFormat::ImageDirectory);
    assert_eq!(
        inspection
            .entries
            .iter()
            .map(|entry| (entry.id.as_str(), entry.name.as_str()))
            .collect::<Vec<_>>(),
        vec![("0", "boot.img")]
    );

    // 全量列表时代 boot=0、super=1；受控后索引 1 必须越界失败。
    let error = FirmwareExtractService::extract_local(&root, &["1".to_string()], &root.join("out"))
        .expect_err("non-managed directory selection must be rejected");
    assert!(matches!(
        error,
        nwflash_application::FirmwareExtractApplicationError::InvalidSelection
    ));

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
