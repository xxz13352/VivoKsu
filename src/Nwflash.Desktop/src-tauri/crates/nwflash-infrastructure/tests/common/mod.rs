//! Shared test fixtures for nwflash-infrastructure integration tests.
#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;

/// Spawn an HTTP server that serves `data` with Range support.
/// Replies with 200 for non-Range requests and 206 + Content-Range for valid Range
/// requests; 416 for out-of-bounds ranges. Returns `http://127.0.0.1:<port>/`.
pub fn spawn_range_server(data: Vec<u8>) -> String {
    let data = Arc::new(data);
    let listener = TcpListener::bind("127.0.0.1:0").expect("range server should bind");
    let addr = listener
        .local_addr()
        .expect("bound address should be known");
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let data = Arc::clone(&data);
            thread::spawn(move || {
                let _ = serve_connection(&mut stream, &data);
            });
        }
    });
    format!("http://{addr}/")
}

fn serve_connection(stream: &mut TcpStream, data: &[u8]) -> std::io::Result<()> {
    let mut buffer = [0u8; 4096];
    let mut request = Vec::new();
    loop {
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            return Ok(());
        }
        request.extend_from_slice(&buffer[..read]);
        if request.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if request.len() > 64 * 1024 {
            return Ok(());
        }
    }
    let head = String::from_utf8_lossy(&request);
    let range = head
        .lines()
        .find_map(|line| {
            let lower = line.to_ascii_lowercase();
            lower.strip_prefix("range:").map(|v| v.trim().to_string())
        })
        .and_then(|value| value.strip_prefix("bytes=").map(|v| v.to_string()));

    let total = data.len() as u64;
    match range {
        None => {
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {total}\r\nContent-Type: application/octet-stream\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n"
            );
            stream.write_all(header.as_bytes())?;
            stream.write_all(data)?;
            Ok(())
        }
        Some(spec) => {
            let (start, end) = parse_range(&spec);
            match (start, end) {
                (Some(s), Some(e)) if e >= s && (e as u64) < total => {
                    let slice = &data[s..=e];
                    let len = slice.len() as u64;
                    let header = format!(
                        "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {s}-{e}/{total}\r\nContent-Length: {len}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n"
                    );
                    stream.write_all(header.as_bytes())?;
                    stream.write_all(slice)?;
                    Ok(())
                }
                _ => {
                    let header = format!(
                        "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{total}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    );
                    stream.write_all(header.as_bytes())?;
                    Ok(())
                }
            }
        }
    }
}

fn parse_range(spec: &str) -> (Option<usize>, Option<usize>) {
    let (s, e) = spec.split_once('-').unwrap_or((spec, ""));
    let start = s.trim().parse::<usize>().ok();
    let end = e.trim().parse::<usize>().ok();
    (start, end)
}
