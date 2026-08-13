// UDP 传输实现
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::time::Duration;
use tokio::net::UdpSocket;
use crate::error::TransportError;
use crate::transport::{AsyncTransport, TransportStats};
pub const DEFAULT_PORT: u16 = 5554;
pub const PROTOCOL_VERSION: u16 = 1;
pub const MIN_PACKET_SIZE: u16 = 512;
pub const HOST_MAX_PACKET_SIZE: u16 = 8192;
pub const RESPONSE_TIMEOUT_MS: u64 = 500;
pub const MAX_CONNECT_ATTEMPTS: u32 = 4;
pub const MAX_TRANSMISSION_ATTEMPTS: u32 = 120;
const PACKET_HEADER_SIZE: usize = 4;

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketId {
    Error = 0x00,
    Query = 0x01,
    Init = 0x02,
    Fastboot = 0x03,
}

impl PacketId {
    fn from_u8(value: u8) -> Option<Self> {
        match value {
            0x00 => Some(PacketId::Error),
            0x01 => Some(PacketId::Query),
            0x02 => Some(PacketId::Init),
            0x03 => Some(PacketId::Fastboot),
            _ => None,
        }
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketFlag {
    None = 0x00,
    Continuation = 0x01,
}

#[derive(Debug, Clone)]
pub struct UdpPacket {
    pub id: PacketId,
    pub flags: u8,
    pub sequence: u16,
    pub data: Vec<u8>,
}

impl UdpPacket {
    pub fn new(id: PacketId, sequence: u16, data: Vec<u8>) -> Self {
        Self {
            id,
            flags: 0,
            sequence,
            data,
        }
    }

    pub fn with_continuation(id: PacketId, sequence: u16, data: Vec<u8>) -> Self {
        Self {
            id,
            flags: PacketFlag::Continuation as u8,
            sequence,
            data,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(PACKET_HEADER_SIZE + self.data.len());
        buf.push(self.id as u8);
        buf.push(self.flags);
        buf.extend_from_slice(&self.sequence.to_be_bytes());
        buf.extend_from_slice(&self.data);
        buf
    }

    pub fn decode(data: &[u8]) -> Result<Self, TransportError> {
        if data.len() < PACKET_HEADER_SIZE {
            return Err(TransportError::Protocol("Packet too short".into()));
        }

        let id = PacketId::from_u8(data[0])
            .ok_or_else(|| TransportError::Protocol(format!("Unknown packet ID: {}", data[0])))?;

        let flags = data[1];
        let sequence = u16::from_be_bytes([data[2], data[3]]);
        let payload = data[PACKET_HEADER_SIZE..].to_vec();

        Ok(Self {
            id,
            flags,
            sequence,
            data: payload,
        })
    }

    pub fn is_continuation(&self) -> bool {
        self.flags & (PacketFlag::Continuation as u8) != 0
    }
}
pub struct UdpTransport {
    socket: UdpSocket,
    remote_addr: SocketAddr,
    sequence: u16,
    max_packet_size: u16,
    timeout: Duration,
    stats: TransportStats,
    last_sent: Option<Vec<u8>>,
}

impl UdpTransport {
    pub async fn connect(host: &str, port: u16) -> Result<Self, TransportError> {
        let addr = format!("{}:{}", host, port);
        let remote_addr: SocketAddr = addr
            .parse()
            .map_err(|_| TransportError::Protocol(format!("Invalid address: {}", addr)))?;

        let socket = UdpSocket::bind("0.0.0.0:0")
            .await
            .map_err(TransportError::Io)?;

        socket
            .connect(&remote_addr)
            .await
            .map_err(TransportError::Io)?;

        let mut transport = Self {
            socket,
            remote_addr,
            sequence: 0,
            max_packet_size: MIN_PACKET_SIZE,
            timeout: Duration::from_millis(RESPONSE_TIMEOUT_MS),
            stats: TransportStats::default(),
            last_sent: None,
        };

        transport.initialize().await?;

        Ok(transport)
    }

    async fn initialize(&mut self) -> Result<(), TransportError> {
        let query = UdpPacket::new(PacketId::Query, 0, vec![]);
        let response = self
            .send_packet_with_retry(&query, MAX_CONNECT_ATTEMPTS)
            .await?;

        if response.id != PacketId::Query || response.data.len() < 2 {
            return Err(TransportError::Protocol("Invalid Query response".into()));
        }

        self.sequence = u16::from_be_bytes([response.data[0], response.data[1]]);
        let mut init_data = Vec::with_capacity(4);
        init_data.extend_from_slice(&PROTOCOL_VERSION.to_be_bytes());
        init_data.extend_from_slice(&HOST_MAX_PACKET_SIZE.to_be_bytes());

        let init = UdpPacket::new(PacketId::Init, self.sequence, init_data);
        let response = self
            .send_packet_with_retry(&init, MAX_CONNECT_ATTEMPTS)
            .await?;

        if response.id != PacketId::Init || response.data.len() < 4 {
            return Err(TransportError::Protocol("Invalid Init response".into()));
        }

        let device_version = u16::from_be_bytes([response.data[0], response.data[1]]);
        let device_max_size = u16::from_be_bytes([response.data[2], response.data[3]]);

        self.max_packet_size = std::cmp::min(HOST_MAX_PACKET_SIZE, device_max_size);

        if device_version < PROTOCOL_VERSION {
            return Err(TransportError::Protocol(format!(
                "Unsupported protocol version: {}",
                device_version
            )));
        }

        self.sequence = self.sequence.wrapping_add(1);

        Ok(())
    }

    async fn send_packet_with_retry(
        &mut self,
        packet: &UdpPacket,
        max_attempts: u32,
    ) -> Result<UdpPacket, TransportError> {
        let encoded = packet.encode();
        self.last_sent = Some(encoded.clone());

        for attempt in 0..max_attempts {
            self.socket
                .send(&encoded)
                .await
                .map_err(TransportError::Io)?;
            self.stats.bytes_sent += encoded.len() as u64;
            self.stats.packets_sent += 1;
            let mut buf = vec![0u8; self.max_packet_size as usize];
            match tokio::time::timeout(self.timeout, self.socket.recv(&mut buf)).await {
                Ok(Ok(n)) => {
                    self.stats.bytes_received += n as u64;
                    self.stats.packets_received += 1;

                    let response = UdpPacket::decode(&buf[..n])?;

                    if response.id == PacketId::Error {
                        let error_msg = String::from_utf8_lossy(&response.data);
                        return Err(TransportError::Protocol(error_msg.into_owned()));
                    }

                    if response.sequence == packet.sequence {
                        return Ok(response);
                    }

                }
                Ok(Err(e)) => {
                    self.stats.errors += 1;
                    if attempt == max_attempts - 1 {
                        return Err(TransportError::Io(e));
                    }
                }
                Err(_) => {// 超时，重传
                    self.stats.retransmits += 1;
                    if attempt == max_attempts - 1 {
                        return Err(TransportError::Timeout);
                    }
                }
            }
        }

        Err(TransportError::Timeout)
    }

    pub async fn send_fastboot_data(&mut self, data: &[u8]) -> Result<(), TransportError> {
        let max_data_len = self.max_data_length();

        let mut offset = 0;
        while offset < data.len() {
            let chunk_size = std::cmp::min(max_data_len, data.len() - offset);
            let is_last = offset + chunk_size >= data.len();

            let packet = if is_last {
                UdpPacket::new(
                    PacketId::Fastboot,
                    self.sequence,
                    data[offset..offset + chunk_size].to_vec(),
                )
            } else {
                UdpPacket::with_continuation(
                    PacketId::Fastboot,
                    self.sequence,
                    data[offset..offset + chunk_size].to_vec(),
                )
            };

            self.send_packet_with_retry(&packet, MAX_TRANSMISSION_ATTEMPTS)
                .await?;

            self.sequence = self.sequence.wrapping_add(1);
            offset += chunk_size;
        }

        Ok(())
    }

    pub async fn recv_fastboot_data(&mut self) -> Result<Vec<u8>, TransportError> {
        let mut result = Vec::new();

        loop {
            let request = UdpPacket::new(PacketId::Fastboot, self.sequence, vec![]);
            let response = self
                .send_packet_with_retry(&request, MAX_TRANSMISSION_ATTEMPTS)
                .await?;

            result.extend_from_slice(&response.data);
            self.sequence = self.sequence.wrapping_add(1);

            if !response.is_continuation() {
                break;
            }
        }

        Ok(result)
    }

    pub fn max_data_length(&self) -> usize {
        (self.max_packet_size as usize).saturating_sub(PACKET_HEADER_SIZE)
    }


    pub fn stats(&self) -> &TransportStats {
        &self.stats
    }
}

// （未）AsyncTransport
impl AsyncTransport for UdpTransport {
    fn read<'a>(
        &'a mut self,
        buf: &'a mut [u8],
    ) -> Pin<Box<dyn Future<Output = Result<usize, TransportError>> + Send + 'a>> {
        Box::pin(async move {
            let data = self.recv_fastboot_data().await?;
            let len = std::cmp::min(buf.len(), data.len());
            buf[..len].copy_from_slice(&data[..len]);
            Ok(len)
        })
    }

    fn write<'a>(
        &'a mut self,
        data: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = Result<usize, TransportError>> + Send + 'a>> {
        Box::pin(async move {
            self.send_fastboot_data(data).await?;
            Ok(data.len())
        })
    }

    fn close(&mut self) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>> {
        Box::pin(async move {
            Ok(())
        })
    }

    fn reset(&mut self) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>> {
        Box::pin(async move {
            self.initialize().await
        })
    }

    fn max_packet_size(&self) -> usize {
        self.max_packet_size as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_packet_encode_decode() {
        let packet = UdpPacket::new(PacketId::Fastboot, 0x1234, b"hello".to_vec());
        let encoded = packet.encode();

        assert_eq!(encoded[0], PacketId::Fastboot as u8);
        assert_eq!(encoded[1], 0); // no flags
        assert_eq!(encoded[2], 0x12);
        assert_eq!(encoded[3], 0x34);
        assert_eq!(&encoded[4..], b"hello");

        let decoded = UdpPacket::decode(&encoded).unwrap();
        assert_eq!(decoded.id, PacketId::Fastboot);
        assert_eq!(decoded.sequence, 0x1234);
        assert_eq!(decoded.data, b"hello");
    }

    #[test]
    fn test_continuation_flag() {
        let packet = UdpPacket::with_continuation(PacketId::Fastboot, 0, vec![]);
        assert!(packet.is_continuation());

        let packet = UdpPacket::new(PacketId::Fastboot, 0, vec![]);
        assert!(!packet.is_continuation());
    }

    #[test]
    fn test_packet_id_from_u8() {
        assert_eq!(PacketId::from_u8(0x00), Some(PacketId::Error));
        assert_eq!(PacketId::from_u8(0x01), Some(PacketId::Query));
        assert_eq!(PacketId::from_u8(0x02), Some(PacketId::Init));
        assert_eq!(PacketId::from_u8(0x03), Some(PacketId::Fastboot));
        assert_eq!(PacketId::from_u8(0xFF), None);
    }

    #[test]
    fn test_max_data_length() {
        let max_packet = 1024u16;
        let max_data = (max_packet as usize).saturating_sub(PACKET_HEADER_SIZE);
        assert_eq!(max_data, 1020);
    }
}
