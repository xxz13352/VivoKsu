mod common;

use std::fs;
use std::path::PathBuf;
use std::sync::{atomic::AtomicU64, atomic::Ordering, Arc};
use std::time::{SystemTime, UNIX_EPOCH};

use nwflash_infrastructure::remote_firmware::{
    extract_zip_members, list_zip_members, probe_remote_kind, RangeHttpReader, RemoteFirmwareError,
    RemoteFirmwareKind,
};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

static DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

fn scratch_dir(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "nwflash-remote-fw-{label}-{}-{nonce}",
        DIR_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&root).expect("scratch dir should be created");
    root
}

fn build_zip_bytes(entries: &[(&str, &[u8])], force_zip64: bool, deflate: bool) -> Vec<u8> {
    let mut writer = ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, data) in entries {
        let options = SimpleFileOptions::default()
            .compression_method(if deflate {
                CompressionMethod::Deflated
            } else {
                CompressionMethod::Stored
            })
            .large_file(force_zip64);
        writer.start_file(*name, options).expect("start file");
        std::io::Write::write_all(&mut writer, data).expect("write entry");
    }
    writer.finish().expect("finish zip").into_inner()
}

#[test]
fn range_reader_reports_length_and_reads_multi_chunk_spans() {
    let data: Vec<u8> = (0..(5 * 1024 * 1024)).map(|i| (i % 251) as u8).collect();
    let url = common::spawn_range_server(data.clone());
    let canceled = false;
    let mut is_canceled = || canceled;

    let mut reader = RangeHttpReader::new(&url, None, &mut is_canceled)
        .expect("range reader should open on a 206 server");
    assert_eq!(reader.total_len(), data.len() as u64);

    // 读出开头一批（跨块 CHUNK=1MB）。
    let mut head = [0u8; 8];
    std::io::Read::read_exact(&mut reader, &mut head).expect("read head");
    assert_eq!(&head[..], &data[..8]);

    // seek 到文件尾附近读取 EOCD 区域。
    std::io::Seek::seek(&mut reader, std::io::SeekFrom::End(-64)).expect("seek end");
    let mut tail = [0u8; 64];
    std::io::Read::read_exact(&mut reader, &mut tail).expect("read tail");
    assert_eq!(&tail[..], &data[data.len() - 64..]);

    // 任意偏移读 2MB（跨块）。
    let offset = 2 * 1024 * 1024 - 100;
    std::io::Seek::seek(&mut reader, std::io::SeekFrom::Start(offset as u64)).expect("seek start");
    let mut block = vec![0u8; 2 * 1024 * 1024];
    std::io::Read::read_exact(&mut reader, &mut block).expect("read block");
    assert_eq!(&block[..], &data[offset..offset + 2 * 1024 * 1024]);
}

#[test]
fn range_reader_rejects_a_server_that_ignores_range() {
    // 用一个总是返回 200 全量的服务器：这里用 Range server 但以 URL 传不带范围……
    // 便捷起见，直接指向一个返回 200 的 wiremock。为不引额外依赖，用本地非 Range server：
    // 复用 spawn_range_server 但请求不带 Range 也会 206（探测用）。因此单独造一个 200 server。
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            std::thread::spawn(move || {
                use std::io::{Read, Write};
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let body = b"not-a-range-server-full-body-anyway";
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(header.as_bytes());
                let _ = stream.write_all(body);
            });
        }
    });
    let url = format!("http://{addr}/");
    let canceled = false;
    let mut is_canceled = || canceled;
    match RangeHttpReader::new(&url, None, &mut is_canceled) {
        Ok(_) => panic!("non-range server must be rejected"),
        Err(err) => assert!(matches!(err, RemoteFirmwareError::RangeUnsupported)),
    }
}

#[test]
fn probe_kind_recognizes_each_magic() {
    let canceled = false;

    let crau = common::spawn_range_server(b"CrAU\x01\x00payload".to_vec());
    assert_eq!(
        probe_remote_kind(&crau, None, &mut || canceled).expect("crAU detect"),
        RemoteFirmwareKind::PayloadRaw
    );

    let gzip = common::spawn_range_server(b"\x1f\x8b\x08\x00gzip-data".to_vec());
    assert_eq!(
        probe_remote_kind(&gzip, None, &mut || canceled).expect("gzip detect"),
        RemoteFirmwareKind::Unsupported
    );

    let random = common::spawn_range_server(b"\x99\x88\x77\x66xyz".to_vec());
    assert_eq!(
        probe_remote_kind(&random, None, &mut || canceled).expect("unknown detect"),
        RemoteFirmwareKind::Unsupported
    );
}

#[test]
fn probe_distinguishes_payload_zip_from_direct_image_zip() {
    let payload_zip = build_zip_bytes(
        &[("payload.bin", b"CrAU"), ("care_map.pb", b"map")],
        false,
        false,
    );
    let url = common::spawn_range_server(payload_zip);
    let canceled = false;
    assert_eq!(
        probe_remote_kind(&url, None, &mut || canceled).expect("payload zip"),
        RemoteFirmwareKind::PayloadZip
    );

    let direct = build_zip_bytes(
        &[("boot.img", b"boot"), ("vendor_boot.img", b"vb")],
        false,
        false,
    );
    let url2 = common::spawn_range_server(direct);
    assert_eq!(
        probe_remote_kind(&url2, None, &mut || canceled).expect("direct zip"),
        RemoteFirmwareKind::DirectImageZip
    );
}

#[test]
fn list_zip_members_reports_entries_without_directories() {
    let zip = build_zip_bytes(
        &[
            ("boot.img", b"boot"),
            ("dir/vendor_boot.img", b"vb"),
            ("payload.bin", b"CrAU"),
        ],
        false,
        false,
    );
    let url = common::spawn_range_server(zip);
    let canceled = false;
    let members = list_zip_members(&url, None, &mut || canceled).expect("list");
    let names: Vec<_> = members.iter().map(|m| m.name.as_str()).collect();
    assert!(names.contains(&"boot"));
    assert!(names.contains(&"vendor_boot"));
    assert!(names.contains(&"payload"));
    assert_eq!(members.len(), 3);
}

#[test]
fn extract_direct_zip_fetches_only_wanted_members() {
    let boot = vec![42u8; 300 * 1024];
    let vb = vec![7u8; 200 * 1024];
    let system_wide = vec![9u8; 1024 * 1024];
    let zip = build_zip_bytes(
        &[
            ("boot.img", &boot),
            ("vendor_boot.img", &vb),
            ("system.new.dat.0", &system_wide),
        ],
        false,
        true,
    );
    let url = common::spawn_range_server(zip);
    let out = scratch_dir("extract");
    let canceled = false;
    let extracted = extract_zip_members(
        &url,
        None,
        &["init_boot", "boot", "vendor_boot"],
        &out,
        &mut || canceled,
        &mut |_, _| {},
    )
    .expect("extract");

    assert_eq!(extracted.len(), 2);
    for image in extracted {
        let bytes = fs::read(&image.output_path).expect("image file");
        match image.partition_name.as_str() {
            "boot" => assert_eq!(&bytes[..], &boot[..]),
            "vendor_boot" => assert_eq!(&bytes[..], &vb[..]),
            other => panic!("unexpected {other}"),
        }
    }
    // 绝不应提取 system.new.dat.0。
    assert!(!out.join("system.new.dat.0.img").exists());
    assert!(!out.join("system.new.dat.0").exists());
    fs::remove_dir_all(&out).ok();
}

#[test]
fn no_wanted_partition_returns_empty_result_and_app_layer_decides() {
    let zip = build_zip_bytes(&[("system.img", b"sys")], false, false);
    let url = common::spawn_range_server(zip);
    let out = scratch_dir("missing");
    let canceled = false;
    let extracted = extract_zip_members(
        &url,
        None,
        &["boot", "vendor_boot"],
        &out,
        &mut || canceled,
        &mut |_, _| {},
    )
    .expect("no wanted members is not an extract error");
    // 无任何命中：返回空结果，由上层（root_ota）判定无 boot 分区并给出引导。
    assert!(extracted.is_empty());
    fs::remove_dir_all(&out).ok();
}

#[test]
fn extract_reports_progress_monotonically() {
    let boot = vec![3u8; 400 * 1024];
    let zip = build_zip_bytes(&[("boot.img", &boot)], false, true);
    let url = common::spawn_range_server(zip);
    let out = scratch_dir("progress");
    let canceled = false;
    let progress: Arc<std::sync::Mutex<Vec<(String, u64)>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = progress.clone();
    extract_zip_members(
        &url,
        None,
        &["boot"],
        &out,
        &mut || canceled,
        &mut move |name, bytes| sink.lock().unwrap().push((name.to_string(), bytes)),
    )
    .expect("extract");

    let events = progress.lock().unwrap();
    assert!(!events.is_empty());
    let last = events.last().unwrap().1;
    assert_eq!(last, boot.len() as u64);
    // 单调不减。
    let mut running = 0;
    for (_, bytes) in events.iter() {
        assert!(*bytes >= running);
        running = *bytes;
    }
    fs::remove_dir_all(&out).ok();
}

#[test]
fn cancellation_aborts_extraction_with_interrupted_read() {
    let boot = vec![5u8; 300 * 1024];
    let zip = build_zip_bytes(&[("boot.img", &boot)], false, false);
    let url = common::spawn_range_server(zip);
    let out = scratch_dir("cancel");
    let canceled = true;
    let result = extract_zip_members(
        &url,
        None,
        &["boot"],
        &out,
        &mut || canceled,
        &mut |_, _| {},
    );
    // 前置取消在 reader 初始化时即返回 Cancelled。
    assert!(matches!(result, Err(RemoteFirmwareError::Cancelled)));
    fs::remove_dir_all(&out).ok();
}

#[test]
fn mid_extraction_cancellation_reports_cancelled_not_archive_error() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    let boot = vec![9u8; 2 * 1024 * 1024];
    let zip = build_zip_bytes(&[("boot.img", &boot)], false, false);
    let url = common::spawn_range_server(zip);
    let out = scratch_dir("midcancel");
    let canceled = Arc::new(AtomicBool::new(false));
    let canceled_for_progress = canceled.clone();
    let result = extract_zip_members(
        &url,
        None,
        &["boot"],
        &out,
        &mut move || canceled.load(Ordering::Acquire),
        &mut move |_name, _bytes| {
            // 首次进度回调后触发取消，模拟提取中途用户停止。
            canceled_for_progress.store(true, Ordering::Release);
        },
    );
    // 中途取消必须被识别为 Cancelled，而非 Archive/InvalidFormat。
    assert!(matches!(result, Err(RemoteFirmwareError::Cancelled)));
    fs::remove_dir_all(&out).ok();
}

#[test]
fn zip64_archive_is_supported() {
    let boot = vec![11u8; 700 * 1024];
    let vb = vec![12u8; 500 * 1024];
    // large_file(true) 强制写出 ZIP64 局部头 + 中央目录（实测 zip 4.6.1 会写 ZIP64）。
    let zip = build_zip_bytes(&[("boot.img", &boot), ("vendor_boot.img", &vb)], true, true);
    let url = common::spawn_range_server(zip);
    let out = scratch_dir("zip64");
    let canceled = false;
    let extracted = extract_zip_members(
        &url,
        None,
        &["boot", "vendor_boot"],
        &out,
        &mut || canceled,
        &mut |_, _| {},
    )
    .expect("zip64 extract should work");
    assert_eq!(extracted.len(), 2);
    let names: Vec<_> = extracted
        .iter()
        .map(|i| i.partition_name.as_str())
        .collect();
    assert!(names.contains(&"boot"));
    assert!(names.contains(&"vendor_boot"));
    for image in extracted {
        match image.partition_name.as_str() {
            "boot" => assert_eq!(image.size_bytes, boot.len() as i64),
            "vendor_boot" => assert_eq!(image.size_bytes, vb.len() as i64),
            other => panic!("unexpected {other}"),
        }
    }
    fs::remove_dir_all(&out).ok();
}
