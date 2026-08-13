//未完成异步
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use async_channel::{Receiver, Sender};
use bytes::Bytes;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::error::TransportError; 

pub trait AsyncTransport: Send + Sync { 
 
    fn read<'a>(
        &'a mut self,
        buf: &'a mut [u8],
    ) -> Pin<Box<dyn Future<Output = Result<usize, TransportError>> + Send + 'a>>; 
 
    fn write<'a>(
        &'a mut self,
        data: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = Result<usize, TransportError>> + Send + 'a>>; 
    fn close(&mut self) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>>; 
    fn reset(&mut self) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>>;
    fn reinitialize(
        &mut self,
    ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>> { 
        self.reset()
    } 
    fn set_timeout(&mut self, _timeout: Duration) { 
    } 
    fn wait_for_disconnect(
        &mut self,
    ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>> {
        Box::pin(async { Ok(()) })
    } 
 
    fn max_packet_size(&self) -> usize {
        512 
    } 
    fn supports_bulk_optimization(&self) -> bool {
        false
    }
} 
 
#[allow(async_fn_in_trait)]
pub trait AsyncTransportExt: AsyncTransport { 
 
    async fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), TransportError> {
        let mut total = 0;
        while total < buf.len() {
            let n = self.read(&mut buf[total..]).await?;
            if n == 0 {
                return Err(TransportError::Disconnected);
            }
            total += n;
        }
        Ok(())
    } 
    async fn write_all(&mut self, data: &[u8]) -> Result<(), TransportError> {
        let mut total = 0;
        while total < data.len() {
            let n = self.write(&data[total..]).await?;
            if n == 0 {
                return Err(TransportError::Disconnected);
            }
            total += n;
        }
        Ok(())
    } 
    async fn read_with_timeout(
        &mut self,
        buf: &mut [u8],
        timeout: Duration,
    ) -> Result<usize, TransportError> { 
        self.set_timeout(timeout); 
        tokio::time::timeout(timeout, self.read(buf))
            .await
            .map_err(|_| TransportError::Timeout)?
    } 
    async fn write_with_timeout(
        &mut self,
        data: &[u8],
        timeout: Duration,
    ) -> Result<usize, TransportError> {
        tokio::time::timeout(timeout, self.write(data))
            .await
            .map_err(|_| TransportError::Timeout)?
    }
} 
impl<T: AsyncTransport + ?Sized> AsyncTransportExt for T {} 
 
#[derive(Debug)]
pub struct DataChunk { 
    pub data: Bytes, 
    pub sequence: u64, 
    pub is_last: bool,
} 
#[derive(Debug, Clone)]
pub struct PipelineConfig { 
    pub buffer_count: usize, 
    pub chunk_size: usize, 
    pub prefetch_enabled: bool, 
    pub prefetch_depth: usize,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            buffer_count: 2,
            chunk_size: 1024 * 1024, 
            prefetch_enabled: true,
            prefetch_depth: 4, 
        }
    }
} 

pub struct PipelineSender { 
    tx: Sender<DataChunk>, 
    rx: Receiver<DataChunk>, 
    config: PipelineConfig,
}

impl PipelineSender { 
    pub fn new(config: PipelineConfig) -> Self { 
        let (tx, rx) = async_channel::bounded(config.prefetch_depth);
        Self { tx, rx, config }
    } 
    pub fn sender(&self) -> Sender<DataChunk> {
        self.tx.clone()
    } 
    pub fn receiver(&self) -> Receiver<DataChunk> {
        self.rx.clone()
    } 
    pub fn config(&self) -> &PipelineConfig {
        &self.config
    }
} 
pub struct PipelineReceiver { 
    tx: Sender<DataChunk>, 
    rx: Receiver<DataChunk>, 
    config: PipelineConfig,
}

impl PipelineReceiver { 
    pub fn new(config: PipelineConfig) -> Self {
        let (tx, rx) = async_channel::bounded(config.prefetch_depth);
        Self { tx, rx, config }
    } 
    pub fn sender(&self) -> Sender<DataChunk> {
        self.tx.clone()
    } 
    pub fn receiver(&self) -> Receiver<DataChunk> {
        self.rx.clone()
    }
} 
 
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportType { 
    Usb, 
    Tcp, 
    Udp,
}

impl TransportType {
    pub fn parse(s: &str) -> (Self, &str) {
        if let Some(rest) = s.strip_prefix("tcp:") {
            (TransportType::Tcp, rest)
        } else if let Some(rest) = s.strip_prefix("udp:") {
            (TransportType::Udp, rest)
        } else {
            (TransportType::Usb, s)
        }
    }
} 
 
#[derive(Debug, Default, Clone)]
pub struct TransportStats { 
    pub bytes_sent: u64, 
    pub bytes_received: u64, 
    pub packets_sent: u64, 
    pub packets_received: u64, 
    pub retransmits: u64, 
    pub errors: u64,
}

impl TransportStats { 
    pub fn send_rate(&self, elapsed_secs: f64) -> f64 {
        if elapsed_secs > 0.0 {
            self.bytes_sent as f64 / elapsed_secs
        } else {
            0.0
        }
    } 
    pub fn recv_rate(&self, elapsed_secs: f64) -> f64 {
        if elapsed_secs > 0.0 {
            self.bytes_received as f64 / elapsed_secs
        } else {
            0.0
        }
    }
}
pub trait SyncTransport: Send {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, TransportError>;
    fn write(&mut self, data: &[u8]) -> Result<usize, TransportError>;
    fn close(&mut self) -> Result<(), TransportError>;
    fn reset(&mut self) -> Result<(), TransportError>;
} 

pub struct SyncAdapter<T: AsyncTransport> {
    inner: T,
    runtime: tokio::runtime::Handle,
}

impl<T: AsyncTransport> SyncAdapter<T> {
    pub fn new(inner: T, runtime: tokio::runtime::Handle) -> Self {
        Self { inner, runtime }
    }
}

impl<T: AsyncTransport> SyncTransport for SyncAdapter<T> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, TransportError> {
        self.runtime.block_on(self.inner.read(buf))
    }

    fn write(&mut self, data: &[u8]) -> Result<usize, TransportError> {
        self.runtime.block_on(self.inner.write(data))
    }

    fn close(&mut self) -> Result<(), TransportError> {
        self.runtime.block_on(self.inner.close())
    }

    fn reset(&mut self) -> Result<(), TransportError> {
        self.runtime.block_on(self.inner.reset())
    }
} 

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transport_type_parse() {
        let (t, rest) = TransportType::parse("tcp:192.168.1.1:5555");
        assert_eq!(t, TransportType::Tcp);
        assert_eq!(rest, "192.168.1.1:5555");

        let (t, rest) = TransportType::parse("udp:localhost:5554");
        assert_eq!(t, TransportType::Udp);
        assert_eq!(rest, "localhost:5554");

        let (t, rest) = TransportType::parse("ABC123");
        assert_eq!(t, TransportType::Usb);
        assert_eq!(rest, "ABC123");

        let (t, rest) = TransportType::parse("");
        assert_eq!(t, TransportType::Usb);
        assert_eq!(rest, "");
    }

    #[test]
    fn test_pipeline_config_default() {
        let config = PipelineConfig::default();
        assert_eq!(config.buffer_count, 2);
        assert_eq!(config.chunk_size, 1024 * 1024);
        assert!(config.prefetch_enabled);
    }

    #[test]
    fn test_transport_stats() {
        let mut stats = TransportStats::default();
        stats.bytes_sent = 1024 * 1024; 
        stats.bytes_received = 512 * 1024; 

        let send_rate = stats.send_rate(1.0);
        assert!((send_rate - 1048576.0).abs() < 0.1);

        let recv_rate = stats.recv_rate(2.0);
        assert!((recv_rate - 262144.0).abs() < 0.1);
    }
}
