//! RootOtaService 云提取编排测试：全部走本机 Range mock server，不依赖真实 OTA 或设备。

use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use nwflash_application::{RootOtaExtractOptions, RootOtaService};
use zip4::write::SimpleFileOptions;
use zip4::{CompressionMethod, ZipWriter};

fn scratch_dir(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("nwflash-root-ota-test-{label}-{nonce}"));
    fs::create_dir_all(&root).expect("scratch dir should be created");
    root
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
            lower.trim().strip_prefix("range:").map(|v| v.trim().to_string())
        })
        .and_then(|v| v.strip_prefix("bytes=").map(|s| s.to_string()));
    let total = data.len() as u64;
    match range {
        None => {
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {total}\r\nConnection: close\r\n\r\n")?;
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
            .start_file(*name, SimpleFileOptions::default().compression_method(CompressionMethod::Stored))
            .expect("start file");
        std::io::Write::write_all(&mut writer, data).expect("write entry");
    }
    writer.finish().expect("finish").into_inner()
}

fn staging() -> PathBuf {
    RootOtaService::create_staging_root().expect("staging root")
}

#[test]
fn payload_kind_requires_payload_dumper() {
    let zip = build_zip(&[("payload.bin", b"CrAU\x01"), ("care_map.pb", b"map")]);
    let url = range_server(zip);
    let root = staging();
    let canceled = false;
    let err = RootOtaService::new()
        .extract(
            RootOtaExtractOptions {
                url: &url,
                payload_dumper: None,
                staging_root: &root,
            },
            || canceled,
            |_| {},
            |_| {},
        )
        .expect_err("payload kind without payload_dumper must error");
    assert!(err.to_string().contains("payload"));
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
    let images = RootOtaService::new()
        .extract(
            RootOtaExtractOptions {
                url: &url,
                payload_dumper: None,
                staging_root: &root,
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
                payload_dumper: None,
                staging_root: &root,
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
fn unsupported_kind_errors() {
    let url = range_server(b"\x1f\x8b\x08\x00gzip".to_vec());
    let root = staging();
    let canceled = false;
    let err = RootOtaService::new()
        .extract(
            RootOtaExtractOptions {
                url: &url,
                payload_dumper: None,
                staging_root: &root,
            },
            || canceled,
            |_| {},
            |_| {},
        )
        .expect_err("gzip must be unsupported");
    assert!(err.to_string().contains("不支持的 OTA 格式"));
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
                payload_dumper: None,
                staging_root: &root,
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
                payload_dumper: None,
                staging_root: &root,
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
