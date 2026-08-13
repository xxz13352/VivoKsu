use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use crate::adb::auth::AdbKeyManager;
use crate::adb::protocol::*;
use crate::error::TransportError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Authenticating,
    Connected,
}

#[derive(Debug)]
pub struct AdbStream {
    pub local_id: u32,
    pub remote_id: u32,
    pub destination: String,
    pub is_open: bool,
}

pub struct AdbConnection<T> {
    transport: T,
    state: ConnectionState,
    pub device_banner: String,
    pub max_payload: u32,
    pub protocol_version: u32,
    streams: HashMap<u32, AdbStream>,
    next_local_id: AtomicU32,
    key_manager: Option<AdbKeyManager>,
}

impl<T: AdbTransport> AdbConnection<T> {
    pub fn new(transport: T) -> Self {
        Self::new_internal(transport, false, false)
    }

    pub fn new_fast(mut transport: T, _verbose: bool) -> Self {
        Self::new_internal(transport, false, false)
    }

    pub fn new_with_auth_wait(transport: T) -> Self {
        Self::new_internal(transport, true, false)
    }

    fn new_internal(mut transport: T, wait_for_auth: bool, _verbose: bool) -> Self {
        let timeout = if wait_for_auth {
            Duration::from_secs(60)
        } else {
            Duration::from_secs(5)
        };
        transport.set_timeout(timeout);

        Self {
            transport,
            state: ConnectionState::Disconnected,
            device_banner: String::new(),
            max_payload: MAX_PAYLOAD_V1,
            protocol_version: ADB_VERSION,
            streams: HashMap::new(),
            next_local_id: AtomicU32::new(1),
            key_manager: None,
        }
    }

    pub fn connect(&mut self) -> io::Result<()> {
        self.state = ConnectionState::Connecting;

        let banner = "host::features=shell_v2,cmd,stat_v2,ls_v2,fixed_push_mkdir,apex,abb,fixed_push_symlink_timestamp,abb_exec,remount_shell,track_app,sendrecv_v2,sendrecv_v2_brotli,sendrecv_v2_lz4,sendrecv_v2_zstd,sendrecv_v2_dry_run_send,openscreen_mdns";
        let msg = AdbMessage::connect(ADB_VERSION, MAX_PAYLOAD, banner);
        self.send_message(&msg)?;

        let mut auth_token_seq: u32 = 0;
        let mut pubkey_sent = false;

        loop {
            let response = self.recv_message()?;

            match response.command {
                AdbCommand::Cnxn => {
                    self.protocol_version = response.arg0;
                    self.max_payload = response.arg1;
                    self.device_banner = response.data_str();
                    self.state = ConnectionState::Connected;
                    return Ok(());
                }
                AdbCommand::Auth => {
                    self.state = ConnectionState::Authenticating;
                    let auth_type = response.arg0;

                    let (priv_pem, pub_key) = crate::crypto::get_or_create_keys()
                        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;

                    if auth_type != 1 {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "auth protocol error",
                        ));
                    }

                    auth_token_seq = auth_token_seq.saturating_add(1);
                    if auth_token_seq == 1 {
                        let signature = crate::crypto::sign_token(&priv_pem, &response.data)
                            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
                        let auth_msg = AdbMessage::auth(AuthType::Signature, signature);
                        self.send_message(&auth_msg)?;
                    } else if auth_token_seq == 2 && !pubkey_sent {
                        let auth_msg = AdbMessage::auth(AuthType::RsaPublic, pub_key);
                        self.send_message(&auth_msg)?;
                        pubkey_sent = true;
                    } else {
                        let signature = crate::crypto::sign_token(&priv_pem, &response.data)
                            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
                        let auth_msg = AdbMessage::auth(AuthType::Signature, signature);
                        self.send_message(&auth_msg)?;
                    }
                }
                AdbCommand::Clse => {
                    let msg = AdbMessage::connect(ADB_VERSION, MAX_PAYLOAD, banner);
                    self.send_message(&msg)?;
                }
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("意外响应: {:?}", response.command),
                    ));
                }
            }
        }
    }

    pub fn open_stream(&mut self, destination: &str) -> io::Result<u32> {
        let local_id = self.next_local_id.fetch_add(1, Ordering::SeqCst);

        let msg = AdbMessage::open(local_id, destination);
        self.send_message(&msg)?;

        loop {
            let response = self.recv_message()?;

            match response.command {
                AdbCommand::Okay => {
                    if response.arg1 == local_id {
                        let stream = AdbStream {
                            local_id,
                            remote_id: response.arg0,
                            destination: destination.to_string(),
                            is_open: true,
                        };
                        self.streams.insert(local_id, stream);
                        return Ok(local_id);
                    }
                }
                AdbCommand::Clse => {
                    if response.arg1 == local_id {
                        return Err(io::Error::new(
                            io::ErrorKind::ConnectionRefused,
                            format!("流拒绝: {}", destination),
                        ));
                    }
                }
                _ => {}
            }
        }
    }

    pub fn write_stream(&mut self, local_id: u32, data: &[u8]) -> io::Result<()> {
        let stream = self
            .streams
            .get(&local_id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "流不存在"))?;

        if !stream.is_open {
            return Err(io::Error::new(io::ErrorKind::NotConnected, "流关闭"));
        }

        let remote_id = stream.remote_id;

        for chunk in data.chunks(self.max_payload as usize) {
            let msg = AdbMessage::write(local_id, remote_id, chunk.to_vec());
            self.send_message(&msg)?;

            loop {
                let response = self.recv_message()?;
                match response.command {
                    AdbCommand::Okay => {
                        if response.arg1 == local_id {
                            break;
                        }
                    }
                    AdbCommand::Clse => {
                        if response.arg1 == local_id {
                            if let Some(s) = self.streams.get_mut(&local_id) {
                                s.is_open = false;
                            }
                            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "流关闭"));
                        }
                    }
                    _ => {}
                }
            }
        }

        Ok(())
    }

    pub fn read_stream(&mut self, local_id: u32) -> io::Result<Option<Vec<u8>>> {
        let stream = self
            .streams
            .get(&local_id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "流不存在"))?;

        if !stream.is_open {
            return Ok(None);
        }

        let remote_id = stream.remote_id;

        let msg = self.recv_message()?;

        match msg.command {
            AdbCommand::Wrte => {
                if msg.arg1 == local_id {
                    let okay = AdbMessage::okay(local_id, remote_id);
                    self.send_message(&okay)?;
                    return Ok(Some(msg.data));
                }
            }
            AdbCommand::Clse => {
                if msg.arg1 == local_id {
                    if let Some(s) = self.streams.get_mut(&local_id) {
                        s.is_open = false;
                    }
                    return Ok(None);
                }
            }
            _ => {}
        }

        Ok(Some(vec![]))
    }

    pub fn close_stream(&mut self, local_id: u32) -> io::Result<()> {
        let (remote_id, was_open) = match self.streams.get(&local_id) {
            Some(s) => (s.remote_id, s.is_open),
            None => return Ok(()),
        };
        if was_open {
            let msg = AdbMessage::close(local_id, remote_id);
            self.send_message(&msg)?;
            if let Some(s) = self.streams.get_mut(&local_id) {
                s.is_open = false;
            }
            let restore = Duration::from_secs(30);
            self.set_timeout(Duration::from_secs(2));
            for _ in 0..128 {
                match self.recv_message() {
                    Ok(msg) => match msg.command {
                        AdbCommand::Clse if msg.arg1 == local_id => {
                            break;
                        }
                        AdbCommand::Wrte if msg.arg1 == local_id => {
                            let okay = AdbMessage::okay(local_id, msg.arg0);
                            let _ = self.send_message(&okay);
                        }
                        _ => {}
                    },
                    Err(_) => break,
                }
            }
            self.set_timeout(restore);
        }
        self.streams.remove(&local_id);
        Ok(())
    }

    fn send_message(&mut self, msg: &AdbMessage) -> io::Result<()> {
        let header = msg.encode_header();

        self.transport
            .write_all(&header)
            .map_err(|e| transport_error_to_io_error(e))?;

        if !msg.data.is_empty() {
            self.transport
                .write_all(&msg.data)
                .map_err(|e| transport_error_to_io_error(e))?;
        }

        Ok(())
    }

    fn recv_message(&mut self) -> io::Result<AdbMessage> {
        let mut header = [0u8; 24];
        self.transport
            .read_exact(&mut header)
            .map_err(|e| transport_error_to_io_error(e))?;

        let (command, arg0, arg1, data_len, _checksum) = AdbMessage::decode_header(&header)?;

        let mut payload = vec![0u8; data_len as usize];
        if data_len > 0 {
            self.transport
                .read_exact(&mut payload)
                .map_err(|e| transport_error_to_io_error(e))?;
        }

        AdbMessage::decode(&header, &payload)
    }

    pub fn is_connected(&self) -> bool {
        self.state == ConnectionState::Connected
    }

    pub fn set_timeout(&mut self, timeout: Duration) {
        self.transport.set_timeout(timeout);
    }

    pub fn try_read_stream(
        &mut self,
        local_id: u32,
        timeout_ms: u64,
    ) -> io::Result<Option<Vec<u8>>> {
        let stream = self
            .streams
            .get(&local_id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "流不存在"))?;

        if !stream.is_open {
            return Ok(None);
        }

        let remote_id = stream.remote_id;

        self.transport
            .set_timeout(Duration::from_millis(timeout_ms));

        let result = match self.recv_message() {
            Ok(msg) => match msg.command {
                AdbCommand::Wrte => {
                    if msg.arg1 == local_id {
                        let okay = AdbMessage::okay(local_id, remote_id);
                        self.send_message(&okay)?;
                        Ok(Some(msg.data))
                    } else {
                        Ok(Some(vec![]))
                    }
                }
                AdbCommand::Clse => {
                    if msg.arg1 == local_id {
                        if let Some(s) = self.streams.get_mut(&local_id) {
                            s.is_open = false;
                        }
                        Ok(None)
                    } else {
                        Ok(Some(vec![]))
                    }
                }
                _ => Ok(Some(vec![])),
            },
            Err(e) => {
                if e.kind() == io::ErrorKind::TimedOut || e.kind() == io::ErrorKind::WouldBlock {
                    Ok(Some(vec![]))
                } else {
                    Err(e)
                }
            }
        };

        self.transport.set_timeout(Duration::from_secs(30));

        result
    }
}

pub trait AdbTransport {
    fn write_all(&mut self, data: &[u8]) -> Result<(), TransportError>;
    fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), TransportError>;
    fn set_timeout(&mut self, timeout: Duration);
}

fn transport_error_to_io_error(e: TransportError) -> io::Error {
    match e {
        TransportError::Timeout => io::Error::new(io::ErrorKind::TimedOut, "超时"),
        TransportError::Disconnected => io::Error::new(io::ErrorKind::NotConnected, "断开"),
        TransportError::NoLink => io::Error::new(io::ErrorKind::NotConnected, "无链接"),
        TransportError::DeviceBusy => io::Error::new(io::ErrorKind::WouldBlock, "设备忙"),
        _ => io::Error::new(io::ErrorKind::Other, e.to_string()),
    }
}
