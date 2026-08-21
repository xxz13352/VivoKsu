use std::{
    fs::{self, File},
    io::Write,
    sync::atomic::{AtomicUsize, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use flate2::{write::GzEncoder, Compression};
use nwflash_infrastructure::VivoFirmwareExtractor;

#[test]
fn vivo_gzip_tar_lists_and_extracts_selected_images() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be available")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("nwflash-vivo-firmware-{nonce}"));
    fs::create_dir_all(&root).expect("fixture directory should be created");
    let archive_path = root.join("firmware.gz");
    write_gzip_tar(
        &archive_path,
        &[
            ("boot.img", b"boot"),
            ("release/images/vendor.img", b"vendor"),
        ],
    );

    let entries =
        VivoFirmwareExtractor::list(&archive_path).expect("valid gzip tar should list images");
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].name, "boot.img");
    assert_eq!(entries[0].size_bytes, 4);
    assert_eq!(entries[1].name, "vendor.img");
    assert_eq!(entries[1].full_path, "release/images/vendor.img");

    let output = root.join("output");
    let results = VivoFirmwareExtractor::extract(&archive_path, &[entries[0].clone()], &output)
        .expect("selected image should be extracted");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].size_bytes, 4);
    assert_eq!(
        fs::read(output.join("boot.img")).expect("image should exist"),
        b"boot"
    );

    fs::remove_dir_all(root).expect("fixture directory should be removed");
}

#[test]
fn vivo_zstd_tar_lists_and_extracts_selected_images() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be available")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("nwflash-vivo-zstd-{nonce}"));
    fs::create_dir_all(&root).expect("fixture directory should be created");
    let archive_path = root.join("firmware.zst");
    write_zstd_tar(&archive_path, &[("images/boot.img", b"boot")]);

    let entries =
        VivoFirmwareExtractor::list(&archive_path).expect("valid zstd tar should list images");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].full_path, "images/boot.img");

    let output = root.join("output");
    VivoFirmwareExtractor::extract(&archive_path, &entries, &output)
        .expect("selected image should be extracted from zstd tar");
    assert_eq!(
        fs::read(output.join("boot.img")).expect("image should exist"),
        b"boot"
    );

    fs::remove_dir_all(root).expect("fixture directory should be removed");
}

#[test]
fn vivo_gzip_tar_lists_bin_images_and_rejects_duplicate_output_basenames_before_writing() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be available")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("nwflash-vivo-duplicate-{nonce}"));
    fs::create_dir_all(&root).expect("fixture directory should be created");
    let archive_path = root.join("firmware.gz");
    write_gzip_tar(
        &archive_path,
        &[
            ("images/lk.bin", b"lk"),
            ("slot-a/boot.img", b"a"),
            ("slot-b/boot.img", b"b"),
        ],
    );

    let entries = VivoFirmwareExtractor::list(&archive_path).expect("archive should list images");
    assert!(entries.iter().any(|entry| entry.name == "lk.bin"));
    let output = root.join("output");
    let error = VivoFirmwareExtractor::extract(&archive_path, &entries, &output)
        .expect_err("duplicate output basenames must be rejected before extraction");
    assert!(error.to_string().contains("重名"));
    assert!(!output.join("boot.img").exists());
    assert!(!output.join("lk.bin").exists());

    fs::remove_dir_all(root).expect("fixture directory should be removed");
}

#[test]
fn vivo_gzip_tar_parses_base_256_entry_lengths() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be available")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("nwflash-vivo-base256-{nonce}"));
    fs::create_dir_all(&root).expect("fixture directory should be created");
    let archive_path = root.join("firmware.gz");
    write_base_256_gzip_tar(&archive_path, "boot.img", b"boot");

    let entries =
        VivoFirmwareExtractor::list(&archive_path).expect("base-256 tar length should be accepted");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].size_bytes, 4);

    fs::remove_dir_all(root).expect("fixture directory should be removed");
}

#[test]
fn truncated_vivo_entry_keeps_existing_output_and_removes_partial() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be available")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("nwflash-vivo-truncated-{nonce}"));
    fs::create_dir_all(&root).expect("fixture directory should be created");
    let archive_path = root.join("firmware.gz");
    write_truncated_gzip_tar(&archive_path, "boot.img", b"boot");
    let output = root.join("output");
    fs::create_dir_all(&output).expect("output directory should be created");
    fs::write(output.join("boot.img"), b"previous").expect("previous output should be written");

    let selected = vec![nwflash_infrastructure::VivoFirmwareEntry {
        name: "boot.img".to_string(),
        full_path: "boot.img".to_string(),
        size_bytes: 4,
    }];
    let error = VivoFirmwareExtractor::extract(&archive_path, &selected, &output)
        .expect_err("truncated entry must fail");
    assert!(error.to_string().contains("不完整"));
    assert_eq!(
        fs::read(output.join("boot.img")).expect("previous output should remain"),
        b"previous"
    );
    assert!(!output.join("boot.img.partial").exists());

    fs::remove_dir_all(root).expect("fixture directory should be removed");
}

#[test]
fn canceled_vivo_extraction_keeps_existing_output_and_removes_partial() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be available")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("nwflash-vivo-canceled-{nonce}"));
    fs::create_dir_all(&root).expect("fixture directory should be created");
    let archive_path = root.join("firmware.gz");
    write_gzip_tar(&archive_path, &[("boot.img", b"boot")]);
    let output = root.join("output");
    fs::create_dir_all(&output).expect("output directory should be created");
    fs::write(output.join("boot.img"), b"previous").expect("previous output should be written");
    let selected = VivoFirmwareExtractor::list(&archive_path).expect("archive should be listed");

    let error =
        VivoFirmwareExtractor::extract_with_cancel(&archive_path, &selected, &output, || true)
            .expect_err("canceled extraction must fail");

    assert!(error.to_string().contains("取消"));
    assert_eq!(
        fs::read(output.join("boot.img")).expect("previous output should remain"),
        b"previous"
    );
    assert!(!output.join("boot.img.partial").exists());

    fs::remove_dir_all(root).expect("fixture directory should be removed");
}

#[test]
fn vivo_extraction_honors_cancellation_before_publishing_completed_partials() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be available")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("nwflash-vivo-publish-cancel-{nonce}"));
    fs::create_dir_all(&root).expect("fixture directory should be created");
    let archive_path = root.join("firmware.gz");
    write_gzip_tar(&archive_path, &[("boot.img", b"boot")]);
    let output = root.join("output");
    fs::create_dir_all(&output).expect("output directory should be created");
    fs::write(output.join("boot.img"), b"previous").expect("previous output should be written");
    let selected = VivoFirmwareExtractor::list(&archive_path).expect("archive should be listed");
    let checks = AtomicUsize::new(0);

    let error =
        VivoFirmwareExtractor::extract_with_cancel(&archive_path, &selected, &output, || {
            checks.fetch_add(1, Ordering::SeqCst) >= 3
        })
        .expect_err("cancellation before publication must fail the extraction");

    assert!(error.to_string().contains("取消"));
    assert_eq!(
        fs::read(output.join("boot.img")).expect("previous output should remain"),
        b"previous"
    );
    assert!(!output.join("boot.img.partial").exists());

    fs::remove_dir_all(root).expect("fixture directory should be removed");
}

#[test]
fn vivo_gzip_tar_preserves_gnu_long_entry_paths() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be available")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("nwflash-vivo-long-name-{nonce}"));
    fs::create_dir_all(&root).expect("fixture directory should be created");
    let archive_path = root.join("firmware.gz");
    let long_path = format!("{}/boot.img", "release".repeat(20));
    write_long_name_gzip_tar(&archive_path, &long_path, b"boot");

    let entries =
        VivoFirmwareExtractor::list(&archive_path).expect("GNU long-name entry should be listed");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].full_path, long_path);
    assert_eq!(entries[0].name, "boot.img");

    fs::remove_dir_all(root).expect("fixture directory should be removed");
}

#[test]
fn vivo_gzip_tar_preserves_a_gnu_long_name_across_pax_headers() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be available")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("nwflash-vivo-pax-{nonce}"));
    fs::create_dir_all(&root).expect("fixture directory should be created");
    let archive_path = root.join("firmware.gz");
    let long_path = format!("{}/boot.img", "release".repeat(20));
    write_long_name_gzip_tar_with_pax_header(&archive_path, &long_path, b"boot");

    let entries = VivoFirmwareExtractor::list(&archive_path)
        .expect("PAX headers must not consume the preceding GNU long name");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].full_path, long_path);

    fs::remove_dir_all(root).expect("fixture directory should be removed");
}

#[test]
fn vivo_extraction_reports_scan_progress_and_a_terminal_measurement() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be available")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("nwflash-vivo-progress-{nonce}"));
    fs::create_dir_all(&root).expect("fixture directory should be created");
    let archive_path = root.join("firmware.gz");
    write_gzip_tar(
        &archive_path,
        &[
            ("images/super.img", &vec![7; 32 * 1024]),
            ("boot.img", b"boot"),
        ],
    );
    let entries = VivoFirmwareExtractor::list(&archive_path).expect("archive should list images");
    let selected = entries
        .iter()
        .find(|entry| entry.name == "boot.img")
        .cloned()
        .expect("boot entry should be available");
    let mut progress = Vec::new();

    VivoFirmwareExtractor::extract_with_cancel_and_progress(
        &archive_path,
        &[selected],
        &root.join("output"),
        || false,
        |update| progress.push(update),
    )
    .expect("selected entry should extract");

    assert!(progress
        .iter()
        .any(|update| update.current_entry == "super.img"));
    let terminal = progress
        .last()
        .expect("terminal progress should be reported");
    assert_eq!(terminal.completed_bytes, terminal.total_bytes);

    fs::remove_dir_all(root).expect("fixture directory should be removed");
}

fn write_gzip_tar(path: &std::path::Path, files: &[(&str, &[u8])]) {
    let file = File::create(path).expect("gzip fixture should be created");
    let mut gzip = GzEncoder::new(file, Compression::default());
    for (name, contents) in files {
        let mut header = [0u8; 512];
        write_ascii(&mut header, 0, name);
        write_octal(&mut header, 124, 12, contents.len() as u64);
        header[156] = b'0';
        gzip.write_all(&header)
            .expect("tar header should be written");
        gzip.write_all(contents)
            .expect("tar content should be written");
        let padding = (512 - (contents.len() % 512)) % 512;
        gzip.write_all(&vec![0; padding])
            .expect("tar padding should be written");
    }
    gzip.write_all(&[0; 1024])
        .expect("tar terminator should be written");
    gzip.finish().expect("gzip fixture should be finalized");
}

fn write_zstd_tar(path: &std::path::Path, files: &[(&str, &[u8])]) {
    let file = File::create(path).expect("zstd fixture should be created");
    let mut zstd =
        zstd::stream::write::Encoder::new(file, 0).expect("zstd fixture encoder should be created");
    for (name, contents) in files {
        write_tar_entry(&mut zstd, name, contents, b'0');
    }
    zstd.write_all(&[0; 1024])
        .expect("tar terminator should be written");
    zstd.finish().expect("zstd fixture should be finalized");
}

fn write_base_256_gzip_tar(path: &std::path::Path, name: &str, contents: &[u8]) {
    let file = File::create(path).expect("gzip fixture should be created");
    let mut gzip = GzEncoder::new(file, Compression::default());
    let mut header = [0u8; 512];
    write_ascii(&mut header, 0, name);
    header[124] = 0x80;
    header[135] = contents.len() as u8;
    header[156] = b'0';
    gzip.write_all(&header)
        .expect("tar header should be written");
    gzip.write_all(contents)
        .expect("tar content should be written");
    gzip.write_all(&vec![0; 508])
        .expect("tar padding should be written");
    gzip.write_all(&[0; 1024])
        .expect("tar terminator should be written");
    gzip.finish().expect("gzip fixture should be finalized");
}

fn write_truncated_gzip_tar(path: &std::path::Path, name: &str, contents: &[u8]) {
    let file = File::create(path).expect("gzip fixture should be created");
    let mut gzip = GzEncoder::new(file, Compression::default());
    let mut header = [0u8; 512];
    write_ascii(&mut header, 0, name);
    write_octal(&mut header, 124, 12, contents.len() as u64);
    header[156] = b'0';
    gzip.write_all(&header)
        .expect("tar header should be written");
    gzip.write_all(&contents[..contents.len() - 1])
        .expect("truncated content should be written");
    gzip.finish().expect("gzip fixture should be finalized");
}

fn write_long_name_gzip_tar(path: &std::path::Path, long_path: &str, contents: &[u8]) {
    let file = File::create(path).expect("gzip fixture should be created");
    let mut gzip = GzEncoder::new(file, Compression::default());
    let long_name = format!("{long_path}\0");
    let mut long_header = [0u8; 512];
    write_ascii(&mut long_header, 0, "././@LongLink");
    write_octal(&mut long_header, 124, 12, long_name.len() as u64);
    long_header[156] = b'L';
    gzip.write_all(&long_header)
        .expect("long-name header should be written");
    gzip.write_all(long_name.as_bytes())
        .expect("long name should be written");
    gzip.write_all(&vec![0; (512 - (long_name.len() % 512)) % 512])
        .expect("long-name padding should be written");

    let mut header = [0u8; 512];
    write_ascii(&mut header, 0, &long_path[..100]);
    write_octal(&mut header, 124, 12, contents.len() as u64);
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

fn write_long_name_gzip_tar_with_pax_header(
    path: &std::path::Path,
    long_path: &str,
    contents: &[u8],
) {
    let file = File::create(path).expect("gzip fixture should be created");
    let mut gzip = GzEncoder::new(file, Compression::default());
    let long_name = format!("{long_path}\0");
    write_tar_entry(&mut gzip, "././@LongLink", long_name.as_bytes(), b'L');
    write_tar_entry(&mut gzip, "PaxHeader", b"20 comment=ignored\n", b'x');
    write_tar_entry(&mut gzip, &long_path[..100], contents, b'0');
    gzip.write_all(&[0; 1024])
        .expect("tar terminator should be written");
    gzip.finish().expect("gzip fixture should be finalized");
}

fn write_tar_entry(writer: &mut impl Write, name: &str, contents: &[u8], type_flag: u8) {
    let mut header = [0u8; 512];
    write_ascii(&mut header, 0, name);
    write_octal(&mut header, 124, 12, contents.len() as u64);
    header[156] = type_flag;
    writer
        .write_all(&header)
        .expect("tar header should be written");
    writer
        .write_all(contents)
        .expect("tar content should be written");
    writer
        .write_all(&vec![0; (512 - (contents.len() % 512)) % 512])
        .expect("tar padding should be written");
}

fn write_ascii(header: &mut [u8; 512], offset: usize, value: &str) {
    let bytes = value.as_bytes();
    header[offset..offset + bytes.len()].copy_from_slice(bytes);
}

fn write_octal(header: &mut [u8; 512], offset: usize, length: usize, value: u64) {
    let text = format!("{:0width$o}\0", value, width = length - 1);
    header[offset..offset + length].copy_from_slice(text.as_bytes());
}
