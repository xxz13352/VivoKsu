//! RootOtaService 云提取编排测试：全部走本机 Range mock server，不依赖真实 OTA 或设备。

use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use nwflash_application::{RootOtaExtractOptions, RootOtaService};
use nwflash_infrastructure::{remote_firmware::RemoteFirmwareIntegrity, OtaDiskSpaceProvider};
use zip4::write::SimpleFileOptions;
use zip4::{CompressionMethod, ZipWriter};

#[derive(Clone)]
struct FixedDiskSpace(u64);

impl OtaDiskSpaceProvider for FixedDiskSpace {
    fn available_bytes(&self, _destination: &Path) -> Result<u64, String> {
        Ok(self.0)
    }
}

struct RecordingDiskSpace {
    queries: Arc<AtomicUsize>,
}

impl OtaDiskSpaceProvider for RecordingDiskSpace {
    fn available_bytes(&self, _destination: &Path) -> Result<u64, String> {
        self.queries.fetch_add(1, Ordering::SeqCst);
        Ok(1)
    }
}

/// 起一个 Range mock server，返回 `http://127.0.0.1:<port>/`。
fn range_server(data: Vec<u8>) -> String {
    let data = Arc::new(data);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let data = Arc::clone(&data);
            std::thread::spawn(move || {
                let _ = serve(&mut stream, &data);
            });
        }
    });
    format!("http://{addr}/")
}

fn serve(stream: &mut TcpStream, data: &[u8]) -> std::io::Result<()> {
    let mut buf = [0u8; 4096];
    let mut req = Vec::new();
    loop {
        let n = stream.read(&mut buf)?;
        if n == 0 {
            return Ok(());
        }
        req.extend_from_slice(&buf[..n]);
        if req.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if req.len() > 64 * 1024 {
            return Ok(());
        }
    }
    let head = String::from_utf8_lossy(&req);
    let range = head
        .lines()
        .find_map(|l| {
            let lower = l.to_ascii_lowercase();
            lower
                .trim()
                .strip_prefix("range:")
                .map(|v| v.trim().to_string())
        })
        .and_then(|v| v.strip_prefix("bytes=").map(|s| s.to_string()));
    let total = data.len() as u64;
    match range {
        None => {
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {total}\r\nConnection: close\r\n\r\n"
            )?;
            stream.write_all(data)?;
        }
        Some(spec) => {
            let (s, e) = spec.split_once('-').unwrap_or((&spec[..], "$"));
            let s_ok = s.trim().parse::<usize>().ok();
            let e_ok = e.trim().parse::<usize>().ok();
            match (s_ok, e_ok) {
                (Some(s), Some(e)) if e >= s && (e as u64) < total => {
                    let slice = &data[s..=e];
                    let len = slice.len() as u64;
                    write!(
                        stream,
                        "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {s}-{e}/{total}\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n"
                    )?;
                    stream.write_all(slice)?;
                }
                _ => {
                    write!(
                        stream,
                        "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{total}\r\nConnection: close\r\n\r\n"
                    )?;
                }
            }
        }
    }
    Ok(())
}

fn build_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer = ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, data) in entries {
        writer
            .start_file(
                *name,
                SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
            )
            .expect("start file");
        std::io::Write::write_all(&mut writer, data).expect("write entry");
    }
    writer.finish().expect("finish").into_inner()
}

/// 构造一个含指定分区的真实 CrAU payload，供远程 mock server 使用。
///
/// 内建解析器取代了外部 `payload_dumper`，所以这些测试必须喂真正的 payload 字节，
/// 而不是一个假装产出文件的 .cmd。
fn build_crau_payload(block_size: u32, partitions: &[(&str, Vec<u8>)]) -> Vec<u8> {
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
    fn ld(field: u32, body: &[u8], out: &mut Vec<u8>) {
        tag(field, 2, out);
        varint(body.len() as u64, out);
        out.extend_from_slice(body);
    }

    let mut data = Vec::new();
    let mut partitions_blob = Vec::new();
    for (name, content) in partitions {
        let data_offset = data.len() as u64;
        data.extend_from_slice(content);

        let mut op = Vec::new();
        tag(1, 0, &mut op);
        varint(0, &mut op);
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
        ld(6, &extent, &mut op);

        let mut info = Vec::new();
        tag(1, 0, &mut info);
        varint(content.len() as u64, &mut info);

        let mut partition = Vec::new();
        ld(1, name.as_bytes(), &mut partition);
        ld(7, &info, &mut partition);
        ld(8, &op, &mut partition);
        ld(13, &partition, &mut partitions_blob);
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

/// 把一个 CrAU payload 包进 zip，模拟真实 OTA 的分发形态。
fn zip_with_payload(payload: &[u8]) -> Vec<u8> {
    let mut buffer = Vec::new();
    {
        let mut zip = ZipWriter::new(std::io::Cursor::new(&mut buffer));
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        zip.start_file("payload.bin", options)
            .expect("start payload.bin");
        zip.write_all(payload).expect("write payload.bin");
        zip.finish().expect("finish zip");
    }
    buffer
}

fn staging() -> PathBuf {
    RootOtaService::create_staging_root().expect("staging root")
}


#[test]
fn payload_kind_no_longer_requires_an_external_tool() {
    // 内建实现取代了外部 payload_dumper：不再有「工具未就绪」这个失败模式。
    // 这里给一个损坏的 payload.bin，期望得到的是**格式错误**而不是「工具缺失」。
    let zip = build_zip(&[
        ("payload.bin", b"not-a-crau-payload"),
        ("care_map.pb", b"map"),
    ]);
    let url = range_server(zip);
    let root = staging();
    let canceled = false;
    let err = RootOtaService::new()
        .extract(
            RootOtaExtractOptions {
                url: &url,
                staging_root: &root,
                integrity: RemoteFirmwareIntegrity::default(),
            },
            || canceled,
            |_| {},
            |_| {},
        )
        .expect_err("损坏的 payload 必须失败");
    let message = err.to_string();
    assert!(
        !message.contains("未就绪"),
        "不应再出现「工具未就绪」：{message}"
    );
    fs::remove_dir_all(&root).ok();
}

#[test]
fn payload_root_extraction_reads_a_remote_payload_in_process() {
    // 远程 zip 内含 payload.bin。内建解析器按 Range 直读，不下载整包。
    let boot = vec![0x42u8; 64 * 1024];
    let vendor_boot = vec![0x77u8; 32 * 1024];
    let payload = build_crau_payload(
        4096,
        &[("boot", boot.clone()), ("vendor_boot", vendor_boot.clone())],
    );
    let url = range_server(zip_with_payload(&payload));
    let root = staging();
    let canceled = false;

    let images = RootOtaService::new()
        .extract(
            RootOtaExtractOptions {
                url: &url,
                staging_root: &root,
                integrity: RemoteFirmwareIntegrity::default(),
            },
            || canceled,
            |_| {},
            |_| {},
        )
        .expect("远程 payload 应当在进程内被解析与提取");

    assert_eq!(images.boot_partition_name, "boot");
    let boot_image = images.boot_image.expect("boot 镜像");
    assert_eq!(
        fs::read(&boot_image.path).expect("读取 boot"),
        boot,
        "远程提取的镜像必须与 payload 内的字节完全一致"
    );
    assert!(images.vendor_boot.is_some(), "vendor_boot 也应被提取");
    fs::remove_dir_all(&root).ok();
}

#[test]
fn direct_zip_extracts_boot_and_vendor_boot() {
    let boot = vec![42u8; 300 * 1024];
    let vb = vec![7u8; 200 * 1024];
    let zip = build_zip(&[("boot.img", &boot), ("vendor_boot.img", &vb)]);
    let url = range_server(zip);
    let root = staging();
    let canceled = false;
    let images =
        RootOtaService::with_disk_space(Arc::new(FixedDiskSpace((boot.len() + vb.len()) as u64)))
            .extract(
                RootOtaExtractOptions {
                    url: &url,
                    staging_root: &root,
                    integrity: RemoteFirmwareIntegrity::default(),
                },
                || canceled,
                |_| {},
                |_| {},
            )
            .expect("direct zip should extract");
    assert_eq!(images.boot_partition_name, "boot");
    let boot_image = images.boot_image.expect("boot image");
    assert_eq!(boot_image.size_bytes, boot.len() as i64);
    assert_eq!(fs::read(&boot_image.path).expect("read boot img"), boot);
    let vb_image = images.vendor_boot.expect("vendor_boot image");
    assert_eq!(vb_image.size_bytes, vb.len() as i64);
    assert_eq!(fs::read(&vb_image.path).expect("read vb img"), vb);
    fs::remove_dir_all(&root).ok();
}

#[test]
fn direct_zip_rejects_insufficient_extraction_capacity_before_creating_images() {
    let boot = vec![42u8; 300 * 1024];
    let vendor_boot = vec![7u8; 200 * 1024];
    let zip = build_zip(&[("boot.img", &boot), ("vendor_boot.img", &vendor_boot)]);
    let url = range_server(zip);
    let root = staging();
    let service = RootOtaService::with_disk_space(Arc::new(FixedDiskSpace(1)));

    let error = service
        .extract(
            RootOtaExtractOptions {
                url: &url,
                staging_root: &root,
                integrity: RemoteFirmwareIntegrity::default(),
            },
            || false,
            |_| {},
            |_| {},
        )
        .expect_err("direct zip must reserve extraction capacity before writing");

    assert!(error.to_string().contains("磁盘空间不足"));
    assert!(!root.join("images").exists());
    fs::remove_dir_all(&root).ok();
}

#[test]
fn direct_zip_cancellation_after_member_listing_precedes_capacity_preflight() {
    let zip = build_zip(&[("boot.img", b"boot"), ("vendor_boot.img", b"vendor_boot")]);
    let url = range_server(zip);
    let root = staging();
    let capacity_queries = Arc::new(AtomicUsize::new(0));
    let service = RootOtaService::with_disk_space(Arc::new(RecordingDiskSpace {
        queries: Arc::clone(&capacity_queries),
    }));
    let cancellation_checks = Arc::new(AtomicUsize::new(0));
    let cancellation_checks_for_extract = Arc::clone(&cancellation_checks);

    let error = service
        .extract(
            RootOtaExtractOptions {
                url: &url,
                staging_root: &root,
                integrity: RemoteFirmwareIntegrity::default(),
            },
            move || cancellation_checks_for_extract.fetch_add(1, Ordering::SeqCst) >= 17,
            |_| {},
            |_| {},
        )
        .expect_err("cancellation after listing must win over capacity validation");

    assert!(error.to_string().contains("取消"));
    assert_eq!(cancellation_checks.load(Ordering::SeqCst), 18);
    assert_eq!(capacity_queries.load(Ordering::SeqCst), 0);
    assert!(!root.join("images").exists());
    fs::remove_dir_all(&root).ok();
}

#[test]
fn direct_zip_prefers_init_boot_over_boot() {
    let boot = vec![1u8; 100 * 1024];
    let init_boot = vec![2u8; 100 * 1024];
    let vb = vec![3u8; 100 * 1024];
    let zip = build_zip(&[
        ("init_boot.img", &init_boot),
        ("boot.img", &boot),
        ("vendor_boot.img", &vb),
    ]);
    let url = range_server(zip);
    let root = staging();
    let canceled = false;
    let images = RootOtaService::new()
        .extract(
            RootOtaExtractOptions {
                url: &url,
                staging_root: &root,
                integrity: RemoteFirmwareIntegrity::default(),
            },
            || canceled,
            |_| {},
            |_| {},
        )
        .expect("extract");
    assert_eq!(images.boot_partition_name, "init_boot");
    assert_eq!(
        images.boot_image.expect("boot image").size_bytes,
        init_boot.len() as i64
    );
    fs::remove_dir_all(&root).ok();
}

#[test]
fn direct_zip_reports_monotonic_fractional_progress_until_completion() {
    let boot = vec![1u8; 2 * 1024 * 1024];
    let vendor_boot = vec![2u8; 2 * 1024 * 1024];
    let zip = build_zip(&[("boot.img", &boot), ("vendor_boot.img", &vendor_boot)]);
    let url = range_server(zip);
    let root = staging();
    let canceled = false;
    let progress = std::sync::Arc::new(std::sync::Mutex::new(Vec::<f64>::new()));
    let sink = progress.clone();
    RootOtaService::new()
        .extract(
            RootOtaExtractOptions {
                url: &url,
                staging_root: &root,
                integrity: RemoteFirmwareIntegrity::default(),
            },
            || canceled,
            |_| {},
            move |value| sink.lock().unwrap().push(value),
        )
        .expect("direct zip should extract");

    let values = progress.lock().unwrap();
    assert!(
        values.len() > 1,
        "progress should be reported during extraction"
    );
    assert!(values.windows(2).all(|pair| pair[1] >= pair[0]));
    assert_eq!(values.last().copied(), Some(1.0));
    fs::remove_dir_all(&root).ok();
}

#[test]
fn unsupported_kind_errors() {
    let url = range_server(b"\x1f\x8b\x08\x00gzip".to_vec());
    let root = staging();
    let canceled = false;
    let err = RootOtaService::new()
        .extract(
            RootOtaExtractOptions {
                url: &url,
                staging_root: &root,
                integrity: RemoteFirmwareIntegrity::default(),
            },
            || canceled,
            |_| {},
            |_| {},
        )
        .expect_err("gzip must be unsupported");
    assert!(err.to_string().contains("不支持的固件格式"));
    fs::remove_dir_all(&root).ok();
}

#[test]
fn missing_boot_partition_errors() {
    let zip = build_zip(&[("system.img", b"sys"), ("vendor.img", b"vendor")]);
    let url = range_server(zip);
    let root = staging();
    let canceled = false;
    let err = RootOtaService::new()
        .extract(
            RootOtaExtractOptions {
                url: &url,
                staging_root: &root,
                integrity: RemoteFirmwareIntegrity::default(),
            },
            || canceled,
            |_| {},
            |_| {},
        )
        .expect_err("no boot image must error");
    assert!(err.to_string().contains("boot"));
    fs::remove_dir_all(&root).ok();
}

#[test]
fn cancellation_aborts_before_extraction() {
    let zip = build_zip(&[("boot.img", b"boot")]);
    let url = range_server(zip);
    let root = staging();
    let canceled = true;
    let err = RootOtaService::new()
        .extract(
            RootOtaExtractOptions {
                url: &url,
                staging_root: &root,
                integrity: RemoteFirmwareIntegrity::default(),
            },
            || canceled,
            |_| {},
            |_| {},
        )
        .expect_err("pre-cancel must abort");
    assert!(err.to_string().contains("取消"));
    fs::remove_dir_all(&root).ok();
}

#[test]
fn staging_roots_are_unique() {
    let a = staging();
    let b = staging();
    assert_ne!(a, b);
    fs::remove_dir_all(a).ok();
    fs::remove_dir_all(b).ok();
}
