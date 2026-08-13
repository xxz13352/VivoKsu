use crate::adb::connection::{AdbConnection, AdbTransport};
use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::Path;  
const SYNC_STAT: &[u8; 4] = b"STAT";
const SYNC_LIST: &[u8; 4] = b"LIST";
const SYNC_SEND: &[u8; 4] = b"SEND";
const SYNC_RECV: &[u8; 4] = b"RECV";
const SYNC_DATA: &[u8; 4] = b"DATA";
const SYNC_DONE: &[u8; 4] = b"DONE";
const SYNC_OKAY: &[u8; 4] = b"OKAY";
const SYNC_FAIL: &[u8; 4] = b"FAIL";
const SYNC_QUIT: &[u8; 4] = b"QUIT";  
const SYNC_DATA_MAX: usize = 64 * 1024;  
#[derive(Debug, Clone)]
pub struct FileInfo {
    pub mode: u32,
    pub size: u32,
    pub mtime: u32,
}  
pub struct SyncSession<'a, T: AdbTransport> {
    conn: &'a mut AdbConnection<T>,
    stream_id: u32,  
    read_buffer: Vec<u8>,
}

impl<'a, T: AdbTransport> SyncSession<'a, T> {  
    pub fn open(conn: &'a mut AdbConnection<T>) -> io::Result<Self> {
        let stream_id = conn.open_stream("sync:")?;
        Ok(Self {
            conn,
            stream_id,
            read_buffer: Vec::new(),
        })
    }  
    fn send_request(&mut self, id: &[u8; 4], path: &str) -> io::Result<()> {
        let path_bytes = path.as_bytes();
        let mut buf = Vec::with_capacity(8 + path_bytes.len());
        buf.extend_from_slice(id);
        buf.write_u32::<LittleEndian>(path_bytes.len() as u32)?;
        buf.extend_from_slice(path_bytes);
        self.conn.write_stream(self.stream_id, &buf)
    }  
    fn read_response(&mut self) -> io::Result<([u8; 4], Vec<u8>)> {  
        let data = self.read_data(8)?;
        let mut id = [0u8; 4];
        id.copy_from_slice(&data[0..4]);
        let len = (&data[4..8]).read_u32::<LittleEndian>()? as usize;  
        let payload = if len > 0 {
            self.read_data(len)?
        } else {
            vec![]
        };

        Ok((id, payload))
    }  
    fn read_data(&mut self, len: usize) -> io::Result<Vec<u8>> {  
        if self.read_buffer.len() >= len {
            let result: Vec<u8> = self.read_buffer.drain(..len).collect();
            return Ok(result);
        }  
        let mut attempts = 0;
        const MAX_ATTEMPTS: usize = 1000;

        while self.read_buffer.len() < len {
            match self.conn.read_stream(self.stream_id)? {
                Some(data) if !data.is_empty() => {
                    self.read_buffer.extend_from_slice(&data);
                    attempts = 0;
                }
                Some(_) => {
                    attempts += 1;
                    if attempts > MAX_ATTEMPTS {
                        return Err(io::Error::new(io::ErrorKind::TimedOut, "读取超时"));
                    }
                }
                None => {
                    return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "连接关闭"));
                }
            }
        }  
        let result: Vec<u8> = self.read_buffer.drain(..len).collect();
        Ok(result)
    }  
    pub fn stat(&mut self, path: &str) -> io::Result<FileInfo> {
        self.send_request(SYNC_STAT, path)?;

        let (id, data) = self.read_response()?;

        if &id == SYNC_STAT && data.len() >= 12 {
            let mut cursor = io::Cursor::new(&data);
            Ok(FileInfo {
                mode: cursor.read_u32::<LittleEndian>()?,
                size: cursor.read_u32::<LittleEndian>()?,
                mtime: cursor.read_u32::<LittleEndian>()?,
            })
        } else if &id == SYNC_FAIL {
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                String::from_utf8_lossy(&data).to_string(),
            ))
        } else {
            Err(io::Error::new(io::ErrorKind::InvalidData, "无效响应"))
        }
    }  
    pub fn push(&mut self, local_path: &Path, remote_path: &str, mode: u32) -> io::Result<()> {
        let mut file = File::open(local_path)?;
        let metadata = file.metadata()?;
        let file_size = metadata.len();  
        let send_path = format!("{},{}", remote_path, mode);
        self.send_request(SYNC_SEND, &send_path)?;  
        let mut buf = vec![0u8; SYNC_DATA_MAX];
        let mut sent = 0u64;

        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }  
            let mut data_msg = Vec::with_capacity(8 + n);
            data_msg.extend_from_slice(SYNC_DATA);
            data_msg.write_u32::<LittleEndian>(n as u32)?;
            data_msg.extend_from_slice(&buf[..n]);
            self.conn.write_stream(self.stream_id, &data_msg)?;

            sent += n as u64;
        }  
        let mtime = metadata
            .modified()
            .map(|t| {
                t.duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as u32
            })
            .unwrap_or(0);

        let mut done_msg = Vec::with_capacity(8);
        done_msg.extend_from_slice(SYNC_DONE);
        done_msg.write_u32::<LittleEndian>(mtime)?;
        self.conn.write_stream(self.stream_id, &done_msg)?;  
        let (id, data) = self.read_response()?;

        if &id == SYNC_OKAY {
            Ok(())
        } else if &id == SYNC_FAIL {
            Err(io::Error::new(
                io::ErrorKind::Other,
                String::from_utf8_lossy(&data).to_string(),
            ))
        } else {
            Err(io::Error::new(io::ErrorKind::InvalidData, "无效响应"))
        }
    }  
    pub fn pull(&mut self, remote_path: &str, local_path: &Path) -> io::Result<u64> {  
        self.send_request(SYNC_RECV, remote_path)?;  
        let mut file = File::create(local_path)?;
        let mut total = 0u64;

        loop {  
            let header = self.read_data(8)?;
            let mut id = [0u8; 4];
            id.copy_from_slice(&header[0..4]);
            let len = (&header[4..8]).read_u32::<LittleEndian>()? as usize;

            if &id == SYNC_DATA {  
                let data = self.read_data(len)?;
                file.write_all(&data)?;
                total += data.len() as u64;
            } else if &id == SYNC_DONE {  
                break;
            } else if &id == SYNC_FAIL {
                let msg = self.read_data(len)?;
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    String::from_utf8_lossy(&msg).to_string(),
                ));
            } else {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "无效响应"));
            }
        }

        Ok(total)
    }  
    pub fn close(mut self) -> io::Result<()> {  
        let mut quit_msg = Vec::with_capacity(8);
        quit_msg.extend_from_slice(SYNC_QUIT);
        quit_msg.write_u32::<LittleEndian>(0)?;
        let _ = self.conn.write_stream(self.stream_id, &quit_msg);

        self.conn.close_stream(self.stream_id)
    }
}  
pub fn push_file<T: AdbTransport>(
    conn: &mut AdbConnection<T>,
    local_path: &Path,
    remote_path: &str,
) -> io::Result<()> {
    let mut session = SyncSession::open(conn)?;  
    let mode = 0o644;
    session.push(local_path, remote_path, mode)?;
    session.close()
}  
pub fn pull_file<T: AdbTransport>(
    conn: &mut AdbConnection<T>,
    remote_path: &str,
    local_path: &Path,
) -> io::Result<u64> {
    let mut session = SyncSession::open(conn)?;
    let size = session.pull(remote_path, local_path)?;
    session.close()?;
    Ok(size)
}
