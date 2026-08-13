use crate::error::FastbootError;
use crate::transport::TransportType;

pub const TCP_DEFAULT_PORT: u16 = 5554;
pub const UDP_DEFAULT_PORT: u16 = 5554;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkSerial {
    pub protocol: TransportType,
    pub host: String,
    pub port: u16,
}

impl NetworkSerial {
    pub fn to_string(&self) -> String {
        let proto = match self.protocol {
            TransportType::Tcp => "tcp",
            TransportType::Udp => "udp",
            TransportType::Usb => "usb",
        };
        format!("{}:{}:{}", proto, self.host, self.port)
    }
}

pub fn parse_network_serial(serial: &str) -> Result<NetworkSerial, FastbootError> {
    let (protocol, rest) = if let Some(rest) = serial.strip_prefix("tcp:") {
        (TransportType::Tcp, rest)
    } else if let Some(rest) = serial.strip_prefix("udp:") {
        (TransportType::Udp, rest)
    } else {
        return Err(FastbootError::InvalidArg(format!(
            "无效的网络序列号格式: {}。应该是 tcp:host[:port] 或 udp:host[:port]",
            serial
        )));
    };
    let (host, port) = parse_host_port(rest, protocol)?;
    if host.is_empty() {
        return Err(FastbootError::InvalidArg("主机地址不能为空".to_string()));
    }

    Ok(NetworkSerial {
        protocol,
        host,
        port,
    })
}

fn parse_host_port(s: &str, protocol: TransportType) -> Result<(String, u16), FastbootError> {
    if s.starts_with('[') {
        let bracket_end = s.find(']').ok_or_else(|| {
            FastbootError::InvalidArg(format!("ipv6 parse error: missing ]: {}", s))
        })?;

        let ipv6_host = &s[1..bracket_end];
        let after_bracket = &s[bracket_end + 1..];

        if after_bracket.is_empty() {
            Ok((ipv6_host.to_string(), default_port(protocol)))
        } else if let Some(port_str) = after_bracket.strip_prefix(':') {
            let port = parse_port(port_str)?;
            Ok((ipv6_host.to_string(), port))
        } else {
            Err(FastbootError::InvalidArg(format!(
                "ipv6 format error: {}",
                s
            )))
        }
    } else {
        if let Some(colon_pos) = s.rfind(':') {
            let host = &s[..colon_pos];
            let port_str = &s[colon_pos + 1..];

            if port_str.is_empty() {
                Ok((host.to_string(), default_port(protocol)))
            } else {
                let port = parse_port(port_str)?;
                Ok((host.to_string(), port))
            }
        } else {
            Ok((s.to_string(), default_port(protocol)))
        }
    }
}
fn parse_port(s: &str) -> Result<u16, FastbootError> {
    s.parse()
        .map_err(|_| FastbootError::InvalidArg(format!("无效的端口号: {}", s)))
}
fn default_port(protocol: TransportType) -> u16 {
    match protocol {
        TransportType::Tcp => TCP_DEFAULT_PORT,
        TransportType::Udp => UDP_DEFAULT_PORT,
        TransportType::Usb => 0,
    }
}
pub fn is_network_serial(serial: &str) -> bool {
    serial.starts_with("tcp:") || serial.starts_with("udp:")
}

pub fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} bytes", bytes)
    }
}

pub fn format_speed(bytes_per_sec: f64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;

    if bytes_per_sec >= MB {
        format!("{:.2} MB/s", bytes_per_sec / MB)
    } else if bytes_per_sec >= KB {
        format!("{:.2} KB/s", bytes_per_sec / KB)
    } else {
        format!("{:.0} B/s", bytes_per_sec)
    }
}

pub fn format_eta(remaining_bytes: u64, bytes_per_sec: f64) -> String {
    if bytes_per_sec <= 0.0 {
        return "calculating...".to_string();
    }

    let remaining_secs = remaining_bytes as f64 / bytes_per_sec;

    if remaining_secs < 60.0 {
        format!("{:.0}s", remaining_secs)
    } else if remaining_secs < 3600.0 {
        let mins = (remaining_secs / 60.0).floor();
        let secs = remaining_secs % 60.0;
        format!("{}m {:.0}s", mins as u32, secs)
    } else {
        let hours = (remaining_secs / 3600.0).floor();
        let mins = ((remaining_secs % 3600.0) / 60.0).floor();
        format!("{}h {}m", hours as u32, mins as u32)
    }
}

pub fn parse_hex_u64(s: &str) -> Result<u64, FastbootError> {
    let s = s.trim();
    let s = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);

    u64::from_str_radix(s, 16)
        .map_err(|_| FastbootError::InvalidArg(format!("无效的十六进制数: {}", s)))
}
pub fn parse_hex_u32(s: &str) -> Result<u32, FastbootError> {
    let s = s.trim();
    let s = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);

    u32::from_str_radix(s, 16)
        .map_err(|_| FastbootError::InvalidArg(format!("无效的十六进制数: {}", s)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_tcp_with_port() {
        let result = parse_network_serial("tcp:192.168.1.1:5555").unwrap();
        assert_eq!(result.protocol, TransportType::Tcp);
        assert_eq!(result.host, "192.168.1.1");
        assert_eq!(result.port, 5555);
    }

    #[test]
    fn test_parse_tcp_without_port() {
        let result = parse_network_serial("tcp:192.168.1.1").unwrap();
        assert_eq!(result.protocol, TransportType::Tcp);
        assert_eq!(result.host, "192.168.1.1");
        assert_eq!(result.port, TCP_DEFAULT_PORT);
    }

    #[test]
    fn test_parse_udp_with_port() {
        let result = parse_network_serial("udp:10.0.0.1:5556").unwrap();
        assert_eq!(result.protocol, TransportType::Udp);
        assert_eq!(result.host, "10.0.0.1");
        assert_eq!(result.port, 5556);
    }

    #[test]
    fn test_parse_udp_without_port() {
        let result = parse_network_serial("udp:localhost").unwrap();
        assert_eq!(result.protocol, TransportType::Udp);
        assert_eq!(result.host, "localhost");
        assert_eq!(result.port, UDP_DEFAULT_PORT);
    }

    #[test]
    fn test_parse_hostname() {
        let result = parse_network_serial("tcp:my-device.local:5554").unwrap();
        assert_eq!(result.host, "my-device.local");
    }

    #[test]
    fn test_parse_ipv6() {
        let result = parse_network_serial("tcp:[::1]:5554").unwrap();
        assert_eq!(result.host, "::1");
        assert_eq!(result.port, 5554);
    }

    #[test]
    fn test_parse_ipv6_without_port() {
        let result = parse_network_serial("tcp:[2001:db8::1]").unwrap();
        assert_eq!(result.host, "2001:db8::1");
        assert_eq!(result.port, TCP_DEFAULT_PORT);
    }

    #[test]
    fn test_parse_invalid_prefix() {
        let result = parse_network_serial("http:192.168.1.1:5554");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_empty_host() {
        let result = parse_network_serial("tcp::5554");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_invalid_port() {
        let result = parse_network_serial("tcp:192.168.1.1:abc");
        assert!(result.is_err());
    }

    #[test]
    fn test_is_network_serial() {
        assert!(is_network_serial("tcp:192.168.1.1"));
        assert!(is_network_serial("udp:localhost:5554"));
        assert!(!is_network_serial("ABC123"));
        assert!(!is_network_serial(""));
    }

    #[test]
    fn test_network_serial_to_string() {
        let serial = NetworkSerial {
            protocol: TransportType::Tcp,
            host: "192.168.1.1".to_string(),
            port: 5554,
        };
        assert_eq!(serial.to_string(), "tcp:192.168.1.1:5554");
    }

    #[test]
    fn test_roundtrip_tcp() {
        let original = "tcp:192.168.1.1:5554";
        let parsed = parse_network_serial(original).unwrap();
        let formatted = parsed.to_string();
        let reparsed = parse_network_serial(&formatted).unwrap();
        assert_eq!(parsed, reparsed);
    }

    #[test]
    fn test_roundtrip_udp() {
        let original = "udp:10.0.0.1:5555";
        let parsed = parse_network_serial(original).unwrap();
        let formatted = parsed.to_string();
        let reparsed = parse_network_serial(&formatted).unwrap();
        assert_eq!(parsed, reparsed);
    }

    #[test]
    fn test_format_size() {
        assert_eq!(format_size(0), "0 bytes");
        assert_eq!(format_size(500), "500 bytes");
        assert_eq!(format_size(1023), "1023 bytes");
        assert_eq!(format_size(1024), "1.00 KB");
        assert_eq!(format_size(1536), "1.50 KB");
        assert_eq!(format_size(1048576), "1.00 MB");
        assert_eq!(format_size(1073741824), "1.00 GB");
    }

    #[test]
    fn test_format_speed() {
        assert_eq!(format_speed(0.0), "0 B/s");
        assert_eq!(format_speed(500.0), "500 B/s");
        assert_eq!(format_speed(1024.0), "1.00 KB/s");
        assert_eq!(format_speed(1048576.0), "1.00 MB/s");
    }

    #[test]
    fn test_format_eta() {
        assert_eq!(format_eta(1024, 1024.0), "1s");
        assert_eq!(format_eta(60 * 1024, 1024.0), "1m 0s");
        assert_eq!(format_eta(3600 * 1024, 1024.0), "1h 0m");
    }

    #[test]
    fn test_parse_hex_u64() {
        assert_eq!(parse_hex_u64("1234").unwrap(), 0x1234);
        assert_eq!(parse_hex_u64("0x1234").unwrap(), 0x1234);
        assert_eq!(parse_hex_u64("0X1234").unwrap(), 0x1234);
        assert_eq!(parse_hex_u64("  0x1234  ").unwrap(), 0x1234);
        assert_eq!(parse_hex_u64("ffffffff").unwrap(), 0xffffffff);
        assert!(parse_hex_u64("xyz").is_err());
    }

    #[test]
    fn test_parse_hex_u32() {
        assert_eq!(parse_hex_u32("1234").unwrap(), 0x1234);
        assert_eq!(parse_hex_u32("0xffffffff").unwrap(), 0xffffffff);
        assert!(parse_hex_u32("xyz").is_err());
    }
}

#[cfg(test)]
mod property_tests {
    use super::*;
    use proptest::prelude::*;
    fn arb_ipv4() -> impl Strategy<Value = String> {
        (0u8..=255, 0u8..=255, 0u8..=255, 0u8..=255)
            .prop_map(|(a, b, c, d)| format!("{}.{}.{}.{}", a, b, c, d))
    }
    fn arb_port() -> impl Strategy<Value = u16> {
        1u16..=65535
    }
    fn arb_protocol() -> impl Strategy<Value = &'static str> {
        prop::sample::select(vec!["tcp", "udp"])
    }

    proptest! {


        #[test]
        fn prop_network_serial_roundtrip(
            protocol in arb_protocol(),
            host in arb_ipv4(),
            port in arb_port()
        ) {
            let serial_str = format!("{}:{}:{}", protocol, host, port);
            let parsed = parse_network_serial(&serial_str).unwrap();
            prop_assert_eq!(&parsed.host, &host);
            prop_assert_eq!(parsed.port, port);
            let formatted = parsed.to_string();
            let reparsed = parse_network_serial(&formatted).unwrap();
            prop_assert_eq!(reparsed.host, host);
            prop_assert_eq!(reparsed.port, port);
        }
        #[test]
        fn prop_default_port_applied(
            protocol in arb_protocol(),
            host in arb_ipv4()
        ) {
            let serial_str = format!("{}:{}", protocol, host);
            let parsed = parse_network_serial(&serial_str).unwrap();
            let expected_port = if protocol == "tcp" { TCP_DEFAULT_PORT } else { UDP_DEFAULT_PORT };
            prop_assert_eq!(parsed.port, expected_port);
        }
        #[test]
        fn prop_is_network_serial_correct(
            protocol in arb_protocol(),
            host in arb_ipv4(),
            port in arb_port()
        ) {
            let serial_str = format!("{}:{}:{}", protocol, host, port);
            prop_assert!(is_network_serial(&serial_str));
            prop_assert!(!is_network_serial(&host));
        }
        #[test]
        fn prop_hex_parse_correct(value in 0u64..0xFFFFFFFF) {
            let hex_lower = format!("0x{:x}", value);
            let hex_upper = format!("0X{:X}", value);
            let hex_no_prefix = format!("{:x}", value);

            prop_assert_eq!(parse_hex_u64(&hex_lower).unwrap(), value);
            prop_assert_eq!(parse_hex_u64(&hex_upper).unwrap(), value);
            prop_assert_eq!(parse_hex_u64(&hex_no_prefix).unwrap(), value);
        }
        #[test]
        fn prop_format_size_units(kb in 1u64..1000, mb in 1u64..1000, gb in 1u64..100) {
            let kb_bytes = kb * 1024;
            let kb_str = format_size(kb_bytes);
            prop_assert!(kb_str.contains("KB") || kb_str.contains("MB") || kb_str.contains("GB"));
            let mb_bytes = mb * 1024 * 1024;
            let mb_str = format_size(mb_bytes);
            prop_assert!(mb_str.contains("MB") || mb_str.contains("GB"));
            let gb_bytes = gb * 1024 * 1024 * 1024;
            let gb_str = format_size(gb_bytes);
            prop_assert!(gb_str.contains("GB"));
        }
    }
}
