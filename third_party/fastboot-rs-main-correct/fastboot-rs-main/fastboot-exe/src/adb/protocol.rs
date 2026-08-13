use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use std::io;   
pub const ADB_VERSION: u32 = 0x01000000;   
pub const ADB_VERSION_MIN: u32 = 0x01000000;   
pub const MAX_PAYLOAD: u32 = 1024 * 1024;   
pub const MAX_PAYLOAD_V1: u32 = 4096;   
   
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum AdbCommand {
    Cnxn = 0x4e584e43,   
    Auth = 0x48545541,   
    Open = 0x4e45504f,   
    Okay = 0x59414b4f,   
    Clse = 0x45534c43,   
    Wrte = 0x45545257,   
}

impl AdbCommand {
    pub fn from_u32(v: u32) -> Option<Self> {
        match v {
            0x4e584e43 => Some(Self::Cnxn),
            0x48545541 => Some(Self::Auth),
            0x4e45504f => Some(Self::Open),
            0x59414b4f => Some(Self::Okay),
            0x45534c43 => Some(Self::Clse),
            0x45545257 => Some(Self::Wrte),
            _ => None,
        }
    }

    pub fn magic(&self) -> u32 {   
        (*self as u32) ^ 0xFFFFFFFF
    }
}   
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum AuthType {
    Token = 1,   
    Signature = 2,   
    RsaPublic = 3,   
}   
#[derive(Debug, Clone)]
pub struct AdbMessage {
    pub command: AdbCommand,
    pub arg0: u32,
    pub arg1: u32,
    pub data: Vec<u8>,
}

impl AdbMessage {
    pub fn new(command: AdbCommand, arg0: u32, arg1: u32, data: Vec<u8>) -> Self {
        Self {
            command,
            arg0,
            arg1,
            data,
        }
    }   
    pub fn connect(version: u32, max_payload: u32, system_identity: &str) -> Self {
        Self::new(
            AdbCommand::Cnxn,
            version,
            max_payload,
            system_identity.as_bytes().to_vec(),
        )
    }   
    pub fn auth(auth_type: AuthType, data: Vec<u8>) -> Self {
        Self::new(AdbCommand::Auth, auth_type as u32, 0, data)
    }   
    pub fn open(local_id: u32, destination: &str) -> Self {   
        let mut data = destination.as_bytes().to_vec();
        data.push(0);
        Self::new(AdbCommand::Open, local_id, 0, data)
    }   
    pub fn okay(local_id: u32, remote_id: u32) -> Self {
        Self::new(AdbCommand::Okay, local_id, remote_id, vec![])
    }   
    pub fn close(local_id: u32, remote_id: u32) -> Self {
        Self::new(AdbCommand::Clse, local_id, remote_id, vec![])
    }   
    pub fn write(local_id: u32, remote_id: u32, data: Vec<u8>) -> Self {
        Self::new(AdbCommand::Wrte, local_id, remote_id, data)
    }   
    fn checksum(data: &[u8]) -> u32 {
        data.iter().map(|&b| b as u32).sum()
    }   
    pub fn encode_header(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(24);

        buf.write_u32::<LittleEndian>(self.command as u32).unwrap();
        buf.write_u32::<LittleEndian>(self.arg0).unwrap();
        buf.write_u32::<LittleEndian>(self.arg1).unwrap();
        buf.write_u32::<LittleEndian>(self.data.len() as u32)
            .unwrap();
        buf.write_u32::<LittleEndian>(Self::checksum(&self.data))
            .unwrap();
        buf.write_u32::<LittleEndian>(self.command.magic()).unwrap();

        buf
    }   
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = self.encode_header();
        buf.extend_from_slice(&self.data);
        buf
    }   
    pub fn decode_header(data: &[u8]) -> io::Result<(AdbCommand, u32, u32, u32, u32)> {
        if data.len() < 24 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "消息头太短"));
        }

        let mut cursor = io::Cursor::new(data);
        let cmd = cursor.read_u32::<LittleEndian>()?;
        let arg0 = cursor.read_u32::<LittleEndian>()?;
        let arg1 = cursor.read_u32::<LittleEndian>()?;
        let data_len = cursor.read_u32::<LittleEndian>()?;
        let checksum = cursor.read_u32::<LittleEndian>()?;
        let magic = cursor.read_u32::<LittleEndian>()?;

        let command = AdbCommand::from_u32(cmd).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("未知命令: 0x{:08x}", cmd),
            )
        })?;   
        if magic != command.magic() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "magic 校验失败"));
        }

        Ok((command, arg0, arg1, data_len, checksum))
    }   
    pub fn decode(header: &[u8], payload: &[u8]) -> io::Result<Self> {
        let (command, arg0, arg1, data_len, checksum) = Self::decode_header(header)?;

        if payload.len() != data_len as usize {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "payload 长度不匹配: 期望 {}, 实际 {}",
                    data_len,
                    payload.len()
                ),
            ));
        }   
        let actual_checksum = Self::checksum(payload);
        if actual_checksum != checksum {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("校验和不匹配: 期望 {}, 实际 {}", checksum, actual_checksum),
            ));
        }

        Ok(Self {
            command,
            arg0,
            arg1,
            data: payload.to_vec(),
        })
    }   
    pub fn data_str(&self) -> String {
        let s = String::from_utf8_lossy(&self.data);
        s.trim_end_matches('\0').to_string()
    }
}   
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceState {
    Offline,   
    Bootloader,   
    Device,   
    Host,   
    Recovery,   
    Sideload,   
    Unauthorized,  
}

impl DeviceState {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "offline" => Self::Offline,
            "bootloader" => Self::Bootloader,
            "device" => Self::Device,
            "host" => Self::Host,
            "recovery" => Self::Recovery,
            "sideload" => Self::Sideload,
            "unauthorized" => Self::Unauthorized,
            _ => Self::Offline,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Offline => "offline",
            Self::Bootloader => "bootloader",
            Self::Device => "device",
            Self::Host => "host",
            Self::Recovery => "recovery",
            Self::Sideload => "sideload",
            Self::Unauthorized => "unauthorized",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_command_magic() {
        assert_eq!(AdbCommand::Cnxn.magic(), 0x4e584e43 ^ 0xFFFFFFFF);
        assert_eq!(AdbCommand::Open.magic(), 0x4e45504f ^ 0xFFFFFFFF);
    }

    #[test]
    fn test_message_encode_decode() {
        let msg = AdbMessage::connect(ADB_VERSION, MAX_PAYLOAD, "host::features=cmd");
        let encoded = msg.encode();

        assert!(encoded.len() >= 24);

        let (cmd, arg0, arg1, data_len, _) = AdbMessage::decode_header(&encoded[..24]).unwrap();
        assert_eq!(cmd, AdbCommand::Cnxn);
        assert_eq!(arg0, ADB_VERSION);
        assert_eq!(arg1, MAX_PAYLOAD);
        assert_eq!(data_len as usize, msg.data.len());
    }

    #[test]
    fn test_checksum() {
        let data = b"hello";
        let sum: u32 = data.iter().map(|&b| b as u32).sum();
        assert_eq!(AdbMessage::checksum(data), sum);
    }
}
