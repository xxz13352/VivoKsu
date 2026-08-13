
use std::io::{self, Read, Write};

pub const ADB_VERSION: u32 = 0x01000000;
pub const ADB_MAX_PAYLOAD: u32 = 256 * 1024;

pub const A_CNXN: u32 = 0x4e584e43;
pub const A_AUTH: u32 = 0x48545541;
pub const A_OPEN: u32 = 0x4e45504f;
pub const A_OKAY: u32 = 0x59414b4f;
pub const A_CLSE: u32 = 0x45534c43;
pub const A_WRTE: u32 = 0x45545257;

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct AdbMessageHeader {
    pub command: u32,
    pub arg0: u32,
    pub arg1: u32,
    pub data_length: u32,
    pub data_checksum: u32,
    pub magic: u32,
}

impl AdbMessageHeader {
    pub fn new(command: u32, arg0: u32, arg1: u32, data_len: u32) -> Self {
        let magic = command ^ 0xFFFFFFFF;
        Self {
            command,
            arg0,
            arg1,
            data_length: data_len,
            data_checksum: 0,
            magic,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(24);
        buf.extend_from_slice(&self.command.to_le_bytes());
        buf.extend_from_slice(&self.arg0.to_le_bytes());
        buf.extend_from_slice(&self.arg1.to_le_bytes());
        buf.extend_from_slice(&self.data_length.to_le_bytes());
        buf.extend_from_slice(&self.data_checksum.to_le_bytes());
        buf.extend_from_slice(&self.magic.to_le_bytes());
        buf
    }

    pub fn decode(data: &[u8]) -> io::Result<Self> {
        if data.len() < 24 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "报头长度不足 24 字节",
            ));
        }
        let command = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        let arg0 = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        let arg1 = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);
        let data_length = u32::from_le_bytes([data[12], data[13], data[14], data[15]]);
        let data_checksum = u32::from_le_bytes([data[16], data[17], data[18], data[19]]);
        let magic = u32::from_le_bytes([data[20], data[21], data[22], data[23]]);

        if magic != (command ^ 0xFFFFFFFF) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "Magic 校验失败"));
        }

        Ok(Self {
            command,
            arg0,
            arg1,
            data_length,
            data_checksum,
            magic,
        })
    }
}

pub fn make_connection_message() -> (AdbMessageHeader, Vec<u8>) {
    let banner = b"host::features=shell_v2,cmd,stat_v2,ls_v2,fixed_push_mkdir,apex,abb,fixed_push_symlink_timestamp,abb_exec,remount_shell,track_app,sendrecv_v2,sendrecv_v2_brotli,sendrecv_v2_lz4,sendrecv_v2_zstd,sendrecv_v2_dry_run_send,openscreen_mdns";
    let header = AdbMessageHeader::new(A_CNXN, ADB_VERSION, ADB_MAX_PAYLOAD, banner.len() as u32);
    (header, banner.to_vec())
}
