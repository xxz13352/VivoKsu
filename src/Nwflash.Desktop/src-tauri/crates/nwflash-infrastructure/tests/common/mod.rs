//! Shared test fixtures for nwflash-infrastructure integration tests.
#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
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

/// 复现 Vivo 固件 CDN 的两种断流行为：
/// - `truncate_after = Some(n)`：206 响应照请求范围声明 `Content-Range` /
///   `Content-Length`，但只发送前 `n` 字节就断开（客户端收到
///   `error decoding response body`）；
/// - `fail_first = k`：前 `k` 个 Range 请求只回响应头、零字节正文就断开，
///   用于逼出「并发 Range 整体失败 → 退化为单连接」路径。
pub fn spawn_unreliable_range_server(
    data: Vec<u8>,
    truncate_after: Option<usize>,
    fail_first: usize,
) -> String {
    let inner = Arc::new(UnreliableRangeServer {
        data,
        truncate_after,
        fail_first,
        range_requests: AtomicUsize::new(0),
    });
    let listener = TcpListener::bind("127.0.0.1:0").expect("unreliable range server should bind");
    let addr = listener
        .local_addr()
        .expect("bound address should be known");
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let inner = Arc::clone(&inner);
            thread::spawn(move || {
                let _ = serve_unreliable_connection(&mut stream, &inner);
            });
        }
    });
    format!("http://{addr}/")
}

struct UnreliableRangeServer {
    data: Vec<u8>,
    truncate_after: Option<usize>,
    fail_first: usize,
    range_requests: AtomicUsize,
}

fn serve_unreliable_connection(
    stream: &mut TcpStream,
    server: &UnreliableRangeServer,
) -> std::io::Result<()> {
    let Some(request) = read_request_head(stream)? else {
        return Ok(());
    };
    let total = server.data.len() as u64;
    if request.starts_with("HEAD ") {
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {total}\r\nContent-Type: application/octet-stream\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n"
        );
        return stream.write_all(header.as_bytes());
    }

    let Some(range) = request_range(&request) else {
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {total}\r\nContent-Type: application/octet-stream\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(header.as_bytes())?;
        return stream.write_all(&server.data);
    };

    let index = server.range_requests.fetch_add(1, Ordering::SeqCst);
    let (Some(start), Some(end)) = range else {
        let header = format!(
            "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{total}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        return stream.write_all(header.as_bytes());
    };
    let slice = &server.data[start..=end];
    let header = format!(
        "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{end}/{total}\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
        slice.len()
    );
    stream.write_all(header.as_bytes())?;
    if index < server.fail_first {
        // 声明了正文长度却一字节不发：客户端只能看到「响应体提前结束」。
        return Ok(());
    }
    let body = match server.truncate_after {
        Some(limit) => &slice[..slice.len().min(limit)],
        None => slice,
    };
    stream.write_all(body)
}

fn read_request_head(stream: &mut TcpStream) -> std::io::Result<Option<String>> {
    let mut buffer = [0u8; 4096];
    let mut request = Vec::new();
    loop {
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            return Ok(None);
        }
        request.extend_from_slice(&buffer[..read]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            return Ok(Some(String::from_utf8_lossy(&request).to_string()));
        }
        if request.len() > 64 * 1024 {
            return Ok(None);
        }
    }
}

fn request_range(head: &str) -> Option<(Option<usize>, Option<usize>)> {
    head.lines()
        .find_map(|line| {
            let lower = line.to_ascii_lowercase();
            lower
                .strip_prefix("range:")
                .map(|value| value.trim().to_string())
        })
        .and_then(|value| value.strip_prefix("bytes=").map(|v| v.to_string()))
        .map(|spec| parse_range(&spec))
}
