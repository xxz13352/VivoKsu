
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

const ADB_SERVER_PORT: u16 = 5037;

pub fn send_request(stream: &mut TcpStream, req: &str) -> io::Result<()> {
    stream.set_nodelay(true)?;
    let hex_len = format!("{:04x}", req.len());
    let msg = format!("{}{}", hex_len, req);
    stream.write_all(msg.as_bytes())?;
    stream.flush()?;
    Ok(())
}

pub fn read_okay(stream: &mut TcpStream) -> io::Result<()> {
    let mut resp = [0u8; 4];
    stream.read_exact(&mut resp)?;
    if &resp != b"OKAY" {
        let msg = match String::from_utf8_lossy(&resp).as_ref() {
            "FAIL" => {
                let mut len_buf = [0u8; 4];
                stream.read_exact(&mut len_buf)?;
                let len = u32::from_le_bytes([len_buf[0], len_buf[1], len_buf[2], len_buf[3]]) as usize;
                let mut fail_msg = vec![0u8; len];
                stream.read_exact(&mut fail_msg)?;
                format!("ADB 服务器拒绝请求: {}", String::from_utf8_lossy(&fail_msg))
            }
            _ => format!("ADB 协议错误: 预期 OKAY 但收到 {:?}", resp),
        };
        eprintln!("{}", msg);
        std::process::exit(1);
    }
    Ok(())
}

pub fn connect_server() -> io::Result<TcpStream> {
    let stream = TcpStream::connect(("127.0.0.1", ADB_SERVER_PORT))?;
    stream.set_nodelay(true)?;
    Ok(stream)
}

pub fn connect_device_transport(serial: Option<&str>) -> io::Result<TcpStream> {
    let mut stream = connect_server()?;
    let payload: String = match serial {
        Some(s) if !s.is_empty() && s != "unknown" => format!("host:transport:{}", s),
        _ => "host:transport-any".to_string(),
    };
    send_request(&mut stream, &payload)?;
    read_okay(&mut stream)?;
    Ok(stream)
}

pub fn send_host_command(cmd: &str) -> io::Result<TcpStream> {
    let mut stream = connect_server()?;
    send_request(&mut stream, cmd)?;
    read_okay(&mut stream)?;
    Ok(stream)
}

pub fn read_hex_length(stream: &mut TcpStream) -> io::Result<usize> {
    let mut hex_buf = [0u8; 4];
    stream.read_exact(&mut hex_buf)?;
    let hex_str = String::from_utf8_lossy(&hex_buf);
    usize::from_str_radix(&hex_str, 16)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "无效的长度头"))
}

pub fn read_exact_bytes(stream: &mut TcpStream, len: usize) -> io::Result<Vec<u8>> {
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf)?;
    Ok(buf)
}
