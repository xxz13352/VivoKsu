pub const FB_CMD_GETVAR: &str = "getvar";
pub const FB_CMD_DOWNLOAD: &str = "download";
pub const FB_CMD_UPLOAD: &str = "upload";
pub const FB_CMD_FLASH: &str = "flash";
pub const FB_CMD_ERASE: &str = "erase";
pub const FB_CMD_BOOT: &str = "boot";
pub const FB_CMD_SET_ACTIVE: &str = "set_active";
pub const FB_CMD_CONTINUE: &str = "continue";
pub const FB_CMD_REBOOT: &str = "reboot";
pub const FB_CMD_SHUTDOWN: &str = "shutdown";
pub const FB_CMD_REBOOT_BOOTLOADER: &str = "reboot-bootloader";
pub const FB_CMD_REBOOT_RECOVERY: &str = "reboot-recovery";
pub const FB_CMD_REBOOT_FASTBOOT: &str = "reboot-fastboot";
pub const FB_CMD_CREATE_PARTITION: &str = "create-logical-partition";
pub const FB_CMD_DELETE_PARTITION: &str = "delete-logical-partition";
pub const FB_CMD_RESIZE_PARTITION: &str = "resize-logical-partition";
pub const FB_CMD_UPDATE_SUPER: &str = "update-super";
pub const FB_CMD_OEM: &str = "oem";
pub const FB_CMD_GSI: &str = "gsi";
pub const FB_CMD_SNAPSHOT_UPDATE: &str = "snapshot-update";
pub const FB_CMD_FETCH: &str = "fetch";
pub const RESPONSE_OKAY: &[u8; 4] = b"OKAY";
pub const RESPONSE_FAIL: &[u8; 4] = b"FAIL";
pub const RESPONSE_DATA: &[u8; 4] = b"DATA";
pub const RESPONSE_INFO: &[u8; 4] = b"INFO";
pub const RESPONSE_TEXT: &[u8; 4] = b"TEXT";
pub const FB_COMMAND_SZ: usize = 4096; 
pub const FB_RESPONSE_SZ: usize = 256; 
pub const FB_RESPONSE_PREFIX_LEN: usize = 4; 
pub const FB_DATA_SIZE_LEN: usize = 8; 
pub const DEFAULT_TIMEOUT_SECS: u64 = 30;
pub const FB_VAR_VERSION: &str = "version";
pub const FB_VAR_VERSION_BOOTLOADER: &str = "version-bootloader";
pub const FB_VAR_VERSION_BASEBAND: &str = "version-baseband";
pub const FB_VAR_PRODUCT: &str = "product";
pub const FB_VAR_SERIALNO: &str = "serialno";
pub const FB_VAR_SECURE: &str = "secure";
pub const FB_VAR_UNLOCKED: &str = "unlocked";
pub const FB_VAR_CURRENT_SLOT: &str = "current-slot";
pub const FB_VAR_MAX_DOWNLOAD_SIZE: &str = "max-download-size";
pub const FB_VAR_HAS_SLOT: &str = "has-slot";
pub const FB_VAR_SLOT_COUNT: &str = "slot-count";
pub const FB_VAR_PARTITION_SIZE: &str = "partition-size";
pub const FB_VAR_PARTITION_TYPE: &str = "partition-type";
pub const FB_VAR_IS_LOGICAL: &str = "is-logical";
pub const FB_VAR_IS_USERSPACE: &str = "is-userspace";
pub const FB_VAR_SUPER_PARTITION_NAME: &str = "super-partition-name";
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response { 
    Okay(String), 
    Fail(String), 
    Data(u32), 
    Info(String), 
    Text(String),
}

impl Response {
    pub fn parse(data: &[u8]) -> Result<(Self, bool), ParseError> { 
        if data.len() < FB_RESPONSE_PREFIX_LEN {
            return Err(ParseError::TooShort {
                expected: FB_RESPONSE_PREFIX_LEN,
                actual: data.len(),
            });
        }

        let prefix = &data[..FB_RESPONSE_PREFIX_LEN];
        let payload_bytes = &data[FB_RESPONSE_PREFIX_LEN..]; 
 
        let payload = extract_payload(payload_bytes);

        match prefix {
            b"OKAY" => Ok((Response::Okay(payload), false)),
            b"FAIL" => Ok((Response::Fail(payload), false)),
            b"DATA" => { 
                let size = parse_data_size(&payload)?;
                Ok((Response::Data(size), false))
            }
            b"INFO" => Ok((Response::Info(payload), true)),
            b"TEXT" => Ok((Response::Text(payload), true)),
            _ => Err(ParseError::UnknownPrefix {
                prefix: prefix.to_vec(),
            }),
        }
    } 

    pub fn encode(&self) -> Vec<u8> {
        match self {
            Response::Okay(s) => encode_with_prefix(b"OKAY", s.as_bytes()),
            Response::Fail(s) => encode_with_prefix(b"FAIL", s.as_bytes()),
            Response::Data(size) => { 
                let size_str = format!("{:08x}", size);
                encode_with_prefix(b"DATA", size_str.as_bytes())
            }
            Response::Info(s) => encode_with_prefix(b"INFO", s.as_bytes()),
            Response::Text(s) => encode_with_prefix(b"TEXT", s.as_bytes()),
        }
    } 
    #[inline]
    pub fn is_okay(&self) -> bool {
        matches!(self, Response::Okay(_))
    } 
    #[inline]
    pub fn is_fail(&self) -> bool {
        matches!(self, Response::Fail(_))
    } 
    #[inline]
    pub fn needs_continue(&self) -> bool {
        matches!(self, Response::Info(_) | Response::Text(_))
    } 

    pub fn message(&self) -> &str {
        match self {
            Response::Okay(s) | Response::Fail(s) | Response::Info(s) | Response::Text(s) => s,
            Response::Data(_) => "",
        }
    } 
    pub fn data_size(&self) -> Option<u32> {
        match self {
            Response::Data(size) => Some(*size),
            _ => None,
        }
    }
} 
 
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError { 
    TooShort { expected: usize, actual: usize }, 
    UnknownPrefix { prefix: Vec<u8> }, 
    InvalidDataSize { raw: String },
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::TooShort { expected, actual } => {
                write!(
                    f,
                    "响应太短: 需要至少 {} 字节，实际 {} 字节",
                    expected, actual
                )
            }
            ParseError::UnknownPrefix { prefix } => {
                write!(f, "未知响应前缀: {:?}", String::from_utf8_lossy(prefix))
            }
            ParseError::InvalidDataSize { raw } => {
                write!(f, "无效的 DATA 大小: '{}'，应该是 8 位十六进制", raw)
            }
        }
    }
}

impl std::error::Error for ParseError {} 
 
fn extract_payload(bytes: &[u8]) -> String { 
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
} 
fn parse_data_size(payload: &str) -> Result<u32, ParseError> { 
 
    u32::from_str_radix(payload, 16).map_err(|_| ParseError::InvalidDataSize {
        raw: payload.to_string(),
    })
} 
fn encode_with_prefix(prefix: &[u8], payload: &[u8]) -> Vec<u8> {
    let mut result = Vec::with_capacity(prefix.len() + payload.len());
    result.extend_from_slice(prefix);
    result.extend_from_slice(payload);
    result
} 

pub fn build_command(cmd: &str, arg: &str) -> String {
    if arg.is_empty() {
        cmd.to_string()
    } else {
        format!("{}:{}", cmd, arg)
    }
} 
pub fn validate_command_length(cmd: &str) -> Result<(), ParseError> {
    if cmd.len() > FB_COMMAND_SZ { 
 
        Err(ParseError::TooShort {
            expected: FB_COMMAND_SZ,
            actual: cmd.len(),
        })
    } else {
        Ok(())
    }
} 

#[cfg(test)]
mod tests {
    use super::*; 
 

    #[test]
    fn test_parse_okay_empty() { 
        let data = b"OKAY";
        let (response, cont) = Response::parse(data).unwrap();
        assert_eq!(response, Response::Okay(String::new()));
        assert!(!cont);
    }

    #[test]
    fn test_parse_okay_with_message() {
        let data = b"OKAY0.4";
        let (response, cont) = Response::parse(data).unwrap();
        assert_eq!(response, Response::Okay("0.4".to_string()));
        assert!(!cont);
    }

    #[test]
    fn test_parse_fail_with_message() {
        let data = b"FAILunknown command";
        let (response, cont) = Response::parse(data).unwrap();
        assert_eq!(response, Response::Fail("unknown command".to_string()));
        assert!(!cont);
    }

    #[test]
    fn test_parse_data_normal() { 
        let data = b"DATA00001234";
        let (response, cont) = Response::parse(data).unwrap();
        assert_eq!(response, Response::Data(0x1234));
        assert!(!cont);
    }

    #[test]
    fn test_parse_data_max() { 
        let data = b"DATAffffffff";
        let (response, cont) = Response::parse(data).unwrap();
        assert_eq!(response, Response::Data(0xFFFFFFFF));
        assert!(!cont);
    }

    #[test]
    fn test_parse_info() {
        let data = b"INFOerasing flash";
        let (response, cont) = Response::parse(data).unwrap();
        assert_eq!(response, Response::Info("erasing flash".to_string()));
        assert!(cont); 
    }

    #[test]
    fn test_parse_text() {
        let data = b"TEXT50%";
        let (response, cont) = Response::parse(data).unwrap();
        assert_eq!(response, Response::Text("50%".to_string()));
        assert!(cont); 
    }

    #[test]
    fn test_parse_with_null_terminator() { 
        let data = b"OKAYtest\0garbage";
        let (response, _) = Response::parse(data).unwrap();
        assert_eq!(response, Response::Okay("test".to_string()));
    }

    #[test]
    fn test_parse_too_short() {
        let data = b"OK";
        let err = Response::parse(data).unwrap_err();
        assert!(matches!(err, ParseError::TooShort { .. }));
    }

    #[test]
    fn test_parse_unknown_prefix() {
        let data = b"WTFxxx";
        let err = Response::parse(data).unwrap_err();
        assert!(matches!(err, ParseError::UnknownPrefix { .. }));
    }

    #[test]
    fn test_parse_invalid_data_size() {
        let data = b"DATAnothex!";
        let err = Response::parse(data).unwrap_err();
        assert!(matches!(err, ParseError::InvalidDataSize { .. }));
    }

    #[test]
    fn test_response_methods() {
        let okay = Response::Okay("test".to_string());
        assert!(okay.is_okay());
        assert!(!okay.is_fail());
        assert!(!okay.needs_continue());
        assert_eq!(okay.message(), "test");
        assert_eq!(okay.data_size(), None);

        let fail = Response::Fail("error".to_string());
        assert!(!fail.is_okay());
        assert!(fail.is_fail());
        assert_eq!(fail.message(), "error");

        let data = Response::Data(1024);
        assert_eq!(data.data_size(), Some(1024));
        assert_eq!(data.message(), "");

        let info = Response::Info("msg".to_string());
        assert!(info.needs_continue());

        let text = Response::Text("txt".to_string());
        assert!(text.needs_continue());
    }

    #[test]
    fn test_build_command() {
        assert_eq!(build_command("getvar", "version"), "getvar:version");
        assert_eq!(build_command("reboot", ""), "reboot");
        assert_eq!(build_command("flash", "boot"), "flash:boot");
    }

    #[test]
    fn test_validate_command_length() { 
        assert!(validate_command_length("getvar:version").is_ok()); 
        let max_cmd = "x".repeat(FB_COMMAND_SZ);
        assert!(validate_command_length(&max_cmd).is_ok()); 
        let too_long = "x".repeat(FB_COMMAND_SZ + 1);
        assert!(validate_command_length(&too_long).is_err());
    }

    #[test]
    fn test_error_display() { 
        let err = ParseError::TooShort {
            expected: 4,
            actual: 2,
        };
        let msg = format!("{}", err);
        assert!(msg.contains("2"));
        assert!(msg.contains("4"));

        let err = ParseError::UnknownPrefix {
            prefix: b"XXXX".to_vec(),
        };
        let msg = format!("{}", err);
        assert!(msg.contains("XXXX"));

        let err = ParseError::InvalidDataSize {
            raw: "notahex".to_string(),
        };
        let msg = format!("{}", err);
        assert!(msg.contains("notahex"));
    }
} 
 
 
 
 
 

#[cfg(test)]
mod property_tests {
    use super::*;
    use proptest::prelude::*; 
 
    fn valid_payload() -> impl Strategy<Value = String> {
        prop::collection::vec(
            prop::char::range('\x01', '\x7f'), 
            0..200, 
        )
        .prop_map(|chars| chars.into_iter().collect())
    } 
    fn any_response() -> impl Strategy<Value = Response> {
        prop_oneof![
            valid_payload().prop_map(Response::Okay),
            valid_payload().prop_map(Response::Fail),
            (0u32..=0xFFFFFFFF).prop_map(Response::Data),
            valid_payload().prop_map(Response::Info),
            valid_payload().prop_map(Response::Text),
        ]
    }

    proptest! { 
 

        #![proptest_config(ProptestConfig::with_cases(256))] 

        #[test]
        fn prop_roundtrip_okay(payload in valid_payload()) {
            let original = Response::Okay(payload);
            let encoded = original.encode();
            let (parsed, cont) = Response::parse(&encoded).unwrap();
            prop_assert_eq!(&original, &parsed);
            prop_assert!(!cont, "OKAY 不应该需要继续等待");
        }

        #[test]
        fn prop_roundtrip_fail(payload in valid_payload()) {
            let original = Response::Fail(payload);
            let encoded = original.encode();
            let (parsed, cont) = Response::parse(&encoded).unwrap();
            prop_assert_eq!(&original, &parsed);
            prop_assert!(!cont, "FAIL 不应该需要继续等待");
        }

        #[test]
        fn prop_roundtrip_data(size: u32) {
            let original = Response::Data(size);
            let encoded = original.encode();
            let (parsed, cont) = Response::parse(&encoded).unwrap();
            prop_assert_eq!(&original, &parsed);
            prop_assert!(!cont, "DATA 不应该需要继续等待");
        }

        #[test]
        fn prop_roundtrip_info(payload in valid_payload()) {
            let original = Response::Info(payload);
            let encoded = original.encode();
            let (parsed, cont) = Response::parse(&encoded).unwrap();
            prop_assert_eq!(&original, &parsed);
            prop_assert!(cont, "INFO 应该需要继续等待");
        }

        #[test]
        fn prop_roundtrip_text(payload in valid_payload()) {
            let original = Response::Text(payload);
            let encoded = original.encode();
            let (parsed, cont) = Response::parse(&encoded).unwrap();
            prop_assert_eq!(&original, &parsed);
            prop_assert!(cont, "TEXT 应该需要继续等待");
        } 
        #[test]
        fn prop_roundtrip_any(response in any_response()) {
            let encoded = response.encode();
            let (parsed, cont) = Response::parse(&encoded).unwrap();
            prop_assert_eq!(&response, &parsed); 
            let expected_cont = response.needs_continue();
            prop_assert_eq!(cont, expected_cont);
        } 

        #[test]
        fn prop_reject_short_input(data in prop::collection::vec(any::<u8>(), 0..4)) { 
            let result = Response::parse(&data);
            prop_assert!(result.is_err());
            if let Err(ParseError::TooShort { expected, actual }) = result {
                prop_assert_eq!(expected, 4);
                prop_assert_eq!(actual, data.len());
            }
        }

        #[test]
        fn prop_reject_invalid_prefix( 
            prefix in prop::collection::vec(any::<u8>(), 4..=4)
                .prop_filter("排除合法前缀", |p| {
                    p != b"OKAY" && p != b"FAIL" && p != b"DATA" &&
                    p != b"INFO" && p != b"TEXT"
                }),
            suffix in prop::collection::vec(any::<u8>(), 0..50)
        ) {
            let mut data = prefix;
            data.extend(suffix);
            let result = Response::parse(&data);
            prop_assert!(result.is_err()); 
            match result {
                Err(ParseError::UnknownPrefix { .. }) => {}
                other => prop_assert!(false, "期望 UnknownPrefix 错误，实际是 {:?}", other),
            }
        } 

        #[test]
        fn prop_data_hex_encoding(size: u32) { 
            let response = Response::Data(size);
            let encoded = response.encode(); 
            prop_assert_eq!(&encoded[..4], b"DATA"); 
            let hex_part = std::str::from_utf8(&encoded[4..]).unwrap();
            prop_assert_eq!(hex_part.len(), 8); 
            let parsed_size = u32::from_str_radix(hex_part, 16).unwrap();
            prop_assert_eq!(size, parsed_size);
        } 
 

        #[test]
        fn prop_command_length_validation(
            cmd in prop::collection::vec(any::<u8>(), 0..5000)
                .prop_map(|v| String::from_utf8_lossy(&v).into_owned())
        ) {
            let result = validate_command_length(&cmd);
            if cmd.len() <= FB_COMMAND_SZ {
                prop_assert!(result.is_ok());
            } else {
                prop_assert!(result.is_err());
            }
        }
    }
}
