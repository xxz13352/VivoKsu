use std::future::Future;
use std::pin::Pin;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use crate::error::TransportError;
use crate::transport::{AsyncTransport, TransportStats};
pub const DEFAULT_PORT: u16 = 5554; 
const HANDSHAKE: &[u8] = b"FB01"; 
const PROTOCOL_VERSION: u8 = 1; 
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2); 
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30); 
const LENGTH_SIZE: usize = 8;
pub struct TcpTransport { 
    stream: TcpStream, 
    message_bytes_left: u64, 
    timeout: Duration, 
    stats: TransportStats,
}

impl TcpTransport { 
    pub async fn connect(host: &str, port: u16) -> Result<Self, TransportError> {
        let addr = format!("{}:{}", host, port); 
        let stream = tokio::time::timeout(HANDSHAKE_TIMEOUT, TcpStream::connect(&addr))
            .await
            .map_err(|_| TransportError::Timeout)?
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::ConnectionRefused {
                    TransportError::ConnectionRefused
                } else {
                    TransportError::Io(e)
                }
            })?; 
        stream.set_nodelay(true).ok();

        let mut transport = Self {
            stream,
            message_bytes_left: 0,
            timeout: DEFAULT_TIMEOUT,
            stats: TransportStats::default(),
        }; 
        transport.handshake().await?;

        Ok(transport)
    } 
 
 
    pub async fn connect_from_string(s: &str) -> Result<Self, TransportError> {
        let (host, port) = Self::parse_address(s)?;
        Self::connect(host, port).await
    } 
    fn parse_address(s: &str) -> Result<(&str, u16), TransportError> {
        if let Some((host, port_str)) = s.rsplit_once(':') {
            let port = port_str
                .parse()
                .map_err(|_| TransportError::Protocol(format!("Invalid port: {}", port_str)))?;
            Ok((host, port))
        } else {
            Ok((s, DEFAULT_PORT))
        }
    } 
    async fn handshake(&mut self) -> Result<(), TransportError> { 
        self.stream
            .write_all(HANDSHAKE)
            .await
            .map_err(TransportError::Io)?; 
        let mut response = [0u8; 4];
        tokio::time::timeout(HANDSHAKE_TIMEOUT, self.stream.read_exact(&mut response))
            .await
            .map_err(|_| TransportError::Timeout)?
            .map_err(TransportError::Io)?; 
        if &response[..2] != b"FB" {
            return Err(TransportError::Protocol(format!(
                "Invalid handshake response: {:?}",
                response
            )));
        } 
        let version = response[2] - b'0';
        if version < PROTOCOL_VERSION {
            return Err(TransportError::Protocol(format!(
                "Unsupported protocol version: {}",
                version
            )));
        }

        Ok(())
    } 
    pub async fn send_message(&mut self, data: &[u8]) -> Result<(), TransportError> { 
        let length = data.len() as u64;
        self.stream
            .write_all(&length.to_be_bytes())
            .await
            .map_err(TransportError::Io)?; 
        self.stream
            .write_all(data)
            .await
            .map_err(TransportError::Io)?;

        self.stats.bytes_sent += LENGTH_SIZE as u64 + data.len() as u64;
        self.stats.packets_sent += 1;

        Ok(())
    } 
    pub async fn recv_message(&mut self) -> Result<Vec<u8>, TransportError> { 
        let mut length_buf = [0u8; LENGTH_SIZE];
        self.stream
            .read_exact(&mut length_buf)
            .await
            .map_err(TransportError::Io)?;

        let length = u64::from_be_bytes(length_buf); 
        if length > 256 * 1024 * 1024 { 
            return Err(TransportError::Protocol(format!(
                "Message too large: {} bytes",
                length
            )));
        } 
        let mut data = vec![0u8; length as usize];
        self.stream
            .read_exact(&mut data)
            .await
            .map_err(TransportError::Io)?;

        self.stats.bytes_received += LENGTH_SIZE as u64 + length;
        self.stats.packets_received += 1;

        Ok(data)
    } 
    pub fn stats(&self) -> &TransportStats {
        &self.stats
    } 
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }
} 

impl AsyncTransport for TcpTransport {
    fn read<'a>(
        &'a mut self,
        buf: &'a mut [u8],
    ) -> Pin<Box<dyn Future<Output = Result<usize, TransportError>> + Send + 'a>> {
        Box::pin(async move { 
            if self.message_bytes_left == 0 {
                let mut length_buf = [0u8; LENGTH_SIZE];
                self.stream
                    .read_exact(&mut length_buf)
                    .await
                    .map_err(TransportError::Io)?;
                self.message_bytes_left = u64::from_be_bytes(length_buf);
            } 
            let to_read = std::cmp::min(buf.len() as u64, self.message_bytes_left) as usize;
            let n = self
                .stream
                .read(&mut buf[..to_read])
                .await
                .map_err(TransportError::Io)?;

            if n == 0 {
                return Err(TransportError::Disconnected);
            }

            self.message_bytes_left -= n as u64;
            self.stats.bytes_received += n as u64;

            Ok(n)
        })
    }

    fn write<'a>(
        &'a mut self,
        data: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = Result<usize, TransportError>> + Send + 'a>> {
        Box::pin(async move {
            self.send_message(data).await?;
            Ok(data.len())
        })
    }

    fn close(&mut self) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>> {
        Box::pin(async move {
            self.stream.shutdown().await.map_err(TransportError::Io)?;
            Ok(())
        })
    }

    fn reset(&mut self) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>> {
        Box::pin(async move { 
            Ok(())
        })
    }

    fn max_packet_size(&self) -> usize { 
        64 * 1024
    }
} 

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_address() {
        let (host, port) = TcpTransport::parse_address("192.168.1.1:5555").unwrap();
        assert_eq!(host, "192.168.1.1");
        assert_eq!(port, 5555);

        let (host, port) = TcpTransport::parse_address("localhost").unwrap();
        assert_eq!(host, "localhost");
        assert_eq!(port, DEFAULT_PORT);

        let (host, port) = TcpTransport::parse_address("[::1]:5554").unwrap();
        assert_eq!(host, "[::1]");
        assert_eq!(port, 5554);
    }

    #[test]
    fn test_length_encoding() { 
        let length: u64 = 0x0000000000001234;
        let bytes = length.to_be_bytes();
        assert_eq!(bytes, [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x12, 0x34]); 
        let decoded = u64::from_be_bytes(bytes);
        assert_eq!(decoded, length);
    }

    #[test]
    fn test_handshake_format() {
        assert_eq!(HANDSHAKE, b"FB01");
        assert_eq!(HANDSHAKE.len(), 4);
    }
}
