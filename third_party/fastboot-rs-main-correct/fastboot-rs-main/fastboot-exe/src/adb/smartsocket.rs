use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

pub fn send_adb_request(stream: &mut TcpStream, req: &str) {
    let _ = stream.set_nodelay(true);

    let payload = format!("{:04x}{}", req.len(), req);
    if stream.write_all(payload.as_bytes()).is_err() {
        eprintln!("与设备的连接意外断开");
        std::process::exit(1);
    }
    if stream.flush().is_err() {
        eprintln!("与设备的连接意外断开");
        std::process::exit(1);
    }

    let mut status = [0u8; 4];
    if stream.read_exact(&mut status).is_err() {
        eprintln!("与设备的连接意外断开");
        std::process::exit(1);
    }
    let status_str = String::from_utf8_lossy(&status);

    if &status != b"OKAY" {
        eprintln!("设备拒绝了当前请求：{}", status_str);
        std::process::exit(1);
    }
}

pub struct AdbSmartSocket {
    stream: TcpStream,
}

impl AdbSmartSocket {
    pub fn new() -> io::Result<Self> {
        let stream = TcpStream::connect("127.0.0.1:5037")?;
        stream.set_read_timeout(Some(Duration::from_secs(30)))?;
        stream.set_write_timeout(Some(Duration::from_secs(30)))?;
        Ok(Self { stream })
    }

    pub fn send_command(&mut self, command: &str) -> io::Result<String> {
        let _ = self.stream.set_nodelay(true);
        let length = format!("{:04x}", command.len());
        let mut buffer = Vec::new();
        buffer.extend_from_slice(length.as_bytes());
        buffer.extend_from_slice(command.as_bytes());
        self.stream.write_all(&buffer)?;
        self.stream.flush()?;

        let mut response = [0u8; 4];
        self.stream.read_exact(&mut response)?;
        let response_str = String::from_utf8_lossy(&response);

        match response_str.as_ref() {
            "OKAY" => {
                let mut length_buf = [0u8; 4];
                self.stream.read_exact(&mut length_buf)?;
                let length_str = String::from_utf8_lossy(&length_buf);
                let length = usize::from_str_radix(&length_str, 16).unwrap_or(0);

                let mut data = vec![0u8; length];
                if length > 0 {
                    self.stream.read_exact(&mut data)?;
                }
                Ok(String::from_utf8_lossy(&data).to_string())
            }
            "FAIL" => {
                let mut length_buf = [0u8; 4];
                self.stream.read_exact(&mut length_buf)?;
                let length_str = String::from_utf8_lossy(&length_buf);
                let length = usize::from_str_radix(&length_str, 16).unwrap_or(0);

                let mut error = vec![0u8; length];
                if length > 0 {
                    self.stream.read_exact(&mut error)?;
                }
                let error_str = String::from_utf8_lossy(&error);
                Err(io::Error::new(io::ErrorKind::Other, error_str.to_string()))
            }
            _ => Err(io::Error::new(
                io::ErrorKind::Other,
                format!("未知响应: {}", response_str),
            )),
        }
    }

    pub fn forward(
        &mut self,
        serial: Option<&str>,
        local: &str,
        remote: &str,
    ) -> io::Result<String> {
        let cmd = if let Some(s) = serial {
            format!("host-serial:{}:forward:{};{}", s, local, remote)
        } else {
            format!("host:forward:{};{}", local, remote)
        };
        self.send_command(&cmd)
    }

    pub fn reverse(
        &mut self,
        serial: Option<&str>,
        remote: &str,
        local: &str,
    ) -> io::Result<String> {
        let cmd = if let Some(s) = serial {
            format!(
                "host-serial:{}:reverse:forward:{};{}",
                s, local, remote
            )
        } else {
            format!("host:reverse:forward:{};{}", local, remote)
        };
        self.send_command(&cmd)
    }

    pub fn tcpip(&mut self, serial: Option<&str>, port: u16) -> io::Result<String> {
        let cmd = if let Some(s) = serial {
            format!("host-serial:{}:tcpip:{}", s, port)
        } else {
            format!("host:tcpip:{}", port)
        };
        self.send_command(&cmd)
    }

    pub fn connect(&mut self, ip: &str, port: u16) -> io::Result<String> {
        let cmd = format!("host:connect:{}:{}", ip, port);
        self.send_command(&cmd)
    }

    pub fn push(
        &mut self,
        serial: Option<&str>,
        local: &std::path::Path,
        remote: &str,
    ) -> io::Result<()> {
        let mut stream = match TcpStream::connect("127.0.0.1:5037") {
            Ok(s) => s,
            Err(_) => {
                eprintln!("与设备的连接意外断开");
                std::process::exit(1);
            }
        };
        let transport_cmd = if let Some(s) = serial {
            format!("host:transport:{}", s)
        } else {
            "host:transport-any".to_string()
        };
        send_adb_request(&mut stream, &transport_cmd);
        send_adb_request(&mut stream, "sync:");

        let path_str = format!("{},33206", remote);
        if stream.write_all(b"SEND").is_err() {
            eprintln!("与设备的连接意外断开");
            std::process::exit(1);
        }
        if stream.flush().is_err() {
            eprintln!("与设备的连接意外断开");
            std::process::exit(1);
        }
        let path_len = (path_str.len() as u32).to_le_bytes();
        if stream.write_all(&path_len).is_err() {
            eprintln!("与设备的连接意外断开");
            std::process::exit(1);
        }
        if stream.flush().is_err() {
            eprintln!("与设备的连接意外断开");
            std::process::exit(1);
        }
        if stream.write_all(path_str.as_bytes()).is_err() {
            eprintln!("与设备的连接意外断开");
            std::process::exit(1);
        }
        if stream.flush().is_err() {
            eprintln!("与设备的连接意外断开");
            std::process::exit(1);
        }

        let mut file = match std::fs::File::open(local) {
            Ok(f) => f,
            Err(_) => {
                eprintln!("找不到指定的文件：{}", local.display());
                std::process::exit(1);
            }
        };
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            let n = match file.read(&mut buffer) {
                Ok(n) => n,
                Err(_) => {
                    eprintln!("与设备的连接意外断开");
                    std::process::exit(1);
                }
            };
            if n == 0 {
                break;
            }
            if stream.write_all(b"DATA").is_err() {
                eprintln!("与设备的连接意外断开");
                std::process::exit(1);
            }
            if stream.flush().is_err() {
                eprintln!("与设备的连接意外断开");
                std::process::exit(1);
            }
            let data_len = (n as u32).to_le_bytes();
            if stream.write_all(&data_len).is_err() {
                eprintln!("与设备的连接意外断开");
                std::process::exit(1);
            }
            if stream.flush().is_err() {
                eprintln!("与设备的连接意外断开");
                std::process::exit(1);
            }
            if stream.write_all(&buffer[..n]).is_err() {
                eprintln!("与设备的连接意外断开");
                std::process::exit(1);
            }
            if stream.flush().is_err() {
                eprintln!("与设备的连接意外断开");
                std::process::exit(1);
            }
        }

        if stream.write_all(b"DONE").is_err() {
            eprintln!("与设备的连接意外断开");
            std::process::exit(1);
        }
        if stream.flush().is_err() {
            eprintln!("与设备的连接意外断开");
            std::process::exit(1);
        }
        let timestamp = 0u32.to_le_bytes();
        if stream.write_all(&timestamp).is_err() {
            eprintln!("与设备的连接意外断开");
            std::process::exit(1);
        }
        if stream.flush().is_err() {
            eprintln!("与设备的连接意外断开");
            std::process::exit(1);
        }

        let mut status = [0u8; 4];
        if stream.read_exact(&mut status).is_err() {
            eprintln!("与设备的连接意外断开");
            std::process::exit(1);
        }
        std::process::exit(0);
    }

    pub fn shell_daemon(&mut self, serial: Option<&str>, command: &str) -> io::Result<()> {
        let mut stream = match TcpStream::connect("127.0.0.1:5037") {
            Ok(s) => s,
            Err(_) => {
                eprintln!("与设备的连接意外断开");
                std::process::exit(1);
            }
        };
        let transport_cmd = if let Some(s) = serial {
            format!("host:transport:{}", s)
        } else {
            "host:transport-any".to_string()
        };
        send_adb_request(&mut stream, &transport_cmd);
        let shell_cmd = format!("shell:{}", command);
        send_adb_request(&mut stream, &shell_cmd);

        let mut buf = [0u8; 4096];
        loop {
            match stream.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let mut stdout = std::io::stdout();
                    if stdout.write_all(&buf[..n]).is_err() {
                        break;
                    }
                    if stdout.flush().is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        std::process::exit(0);
    }

    pub fn shell_interactive(&mut self, serial: Option<&str>) -> io::Result<()> {
        let mut stream = match TcpStream::connect("127.0.0.1:5037") {
            Ok(s) => s,
            Err(_) => {
                eprintln!("与设备的连接意外断开");
                std::process::exit(1);
            }
        };
        let transport_cmd = if let Some(s) = serial {
            format!("host:transport:{}", s)
        } else {
            "host:transport-any".to_string()
        };
        send_adb_request(&mut stream, &transport_cmd);
        send_adb_request(&mut stream, "shell:");

        let stream_in = stream.try_clone()?;
        let stream_out = stream.try_clone()?;

        std::thread::spawn(move || {
            let mut stdin = std::io::stdin();
            let mut stream_in = stream_in;
            let _ = std::io::copy(&mut stdin, &mut stream_in);
        });

        let mut stdout = std::io::stdout();
        let mut stream_out = stream_out;
        let mut buf = [0u8; 1024];
        loop {
            match stream_out.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    stdout.write_all(&buf[..n]).unwrap();
                    stdout.flush().unwrap();
                }
                Err(_) => break,
            }
        }
        std::process::exit(0);
    }
}
