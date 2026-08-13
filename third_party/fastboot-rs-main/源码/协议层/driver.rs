
use std::path::Path;
use std::time::Duration;

use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::error::{FastbootError, RetCode, TransportError};
use crate::protocol::{self, Response, FB_COMMAND_SZ, FB_RESPONSE_SZ};
use crate::transport::{AsyncTransport, AsyncTransportExt};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

const DOWNLOAD_CHUNK_SIZE: usize = 16 * 1024 * 1024;

const UPLOAD_CHUNK_SIZE: usize = 1024 * 1024;

const MAX_RETRY_COUNT: u32 = 3;

const RETRY_DELAY_MS: u64 = 500;

pub type ProgressCallback = Box<dyn Fn(u64, u64) + Send + Sync>;

pub type InfoCallback = Box<dyn Fn(&str) + Send + Sync>;

pub struct DriverCallbacks {
    pub prolog: Box<dyn Fn(&str) + Send + Sync>,
    pub epilog: Box<dyn Fn(RetCode) + Send + Sync>,
    pub info: InfoCallback,
    pub text: Box<dyn Fn(&str) + Send + Sync>,
    pub progress: Option<ProgressCallback>,
}

impl Default for DriverCallbacks {
    fn default() -> Self {
        Self {
            prolog: Box::new(|_| {}),
            epilog: Box::new(|_| {}),
            info: Box::new(|_| {}),
            text: Box::new(|_| {}),
            progress: None,
        }
    }
}

pub struct FastbootDriver<T: AsyncTransport> {
    transport: T,
    callbacks: DriverCallbacks,
    disable_checks: bool,
    timeout: Duration,
    last_error: String,
    max_download_size: Option<u64>,
    auto_retry: bool,
    max_retries: u32,
    consecutive_failures: u32,
}

impl<T: AsyncTransport> FastbootDriver<T> {
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            callbacks: DriverCallbacks::default(),
            disable_checks: false,
            timeout: DEFAULT_TIMEOUT,
            last_error: String::new(),
            max_download_size: None,
            auto_retry: true,
            max_retries: MAX_RETRY_COUNT,
            consecutive_failures: 0,
        }
    }

    pub fn set_callbacks(&mut self, callbacks: DriverCallbacks) {
        self.callbacks = callbacks;
    }

    pub fn set_progress_callback(&mut self, callback: ProgressCallback) {
        self.callbacks.progress = Some(callback);
    }

    pub fn disable_checks(&mut self) {
        self.disable_checks = true;
    }

    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
        self.transport.set_timeout(timeout);
    }

    pub fn set_auto_retry(&mut self, enabled: bool) {
        self.auto_retry = enabled;
    }

    pub fn set_max_retries(&mut self, count: u32) {
        self.max_retries = count;
    }

    pub async fn reset(&mut self) -> Result<(), FastbootError> {
        self.transport.reset().await.map_err(FastbootError::Transport)
    }

    pub async fn deep_reset(&mut self) -> Result<(), FastbootError> {
        self.transport.reinitialize().await.map_err(FastbootError::Transport)
    }

    async fn try_recover(&mut self) -> Result<(), FastbootError> {
        if let Err(e) = self.transport.reset().await {
            self.last_error = format!("Reset 失败: {}", e);
        }

        tokio::time::sleep(Duration::from_millis(RETRY_DELAY_MS)).await;

        let test_result = self.transport.write_all(b"getvar:version").await;
        if test_result.is_err() {
            return Err(FastbootError::Transport(TransportError::NoLink));
        }

        let mut buf = [0u8; 64];
        match tokio::time::timeout(
            Duration::from_secs(2),
            self.transport.read(&mut buf),
        ).await {
            Ok(Ok(_)) => {
                self.consecutive_failures = 0;
                Ok(())
            }
            _ => Err(FastbootError::Transport(TransportError::NoLink)),
        }
    }

    async fn execute_with_retry<F, Fut, R>(&mut self, mut f: F) -> Result<R, FastbootError>
    where
        F: FnMut(&mut Self) -> Fut,
        Fut: std::future::Future<Output = Result<R, FastbootError>>,
    {
        let mut last_error = None;
        let max_attempts = if self.auto_retry { self.max_retries } else { 1 };

        for attempt in 0..max_attempts {
            match f(self).await {
                Ok(result) => {
                    self.consecutive_failures = 0;
                    return Ok(result);
                }
                Err(e) => {
                    self.consecutive_failures += 1;

                    if !e.is_recoverable() || attempt + 1 >= max_attempts {
                        return Err(e);
                    }

                    self.last_error = format!("尝试 {}/{} 失败: {}", attempt + 1, max_attempts, e);
                    last_error = Some(e);

                    if self.try_recover().await.is_err() {
                        return Err(last_error.unwrap());
                    }

                    tokio::time::sleep(Duration::from_millis(RETRY_DELAY_MS * (attempt as u64 + 1))).await;
                }
            }
        }

        Err(last_error.unwrap_or(FastbootError::Timeout))
    }

    pub async fn check_connection(&mut self) -> Result<bool, FastbootError> {
        match self.get_var("version").await {
            Ok(_) => Ok(true),
            Err(FastbootError::Transport(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    pub async fn wait_for_device_ready(
        &mut self,
        max_wait_secs: u64,
        poll_interval_ms: u64,
    ) -> Result<bool, FastbootError> {
        let start = std::time::Instant::now();
        let max_duration = Duration::from_secs(max_wait_secs);
        let poll_interval = Duration::from_millis(poll_interval_ms);

        let original_timeout = self.timeout;

        self.set_timeout(Duration::from_secs(5));

        loop {
            match self.get_var("version").await {
                Ok(_) => {
                    self.set_timeout(original_timeout);
                    return Ok(true);
                }
                Err(FastbootError::Timeout) => {
                    if start.elapsed() >= max_duration {
                        self.set_timeout(original_timeout);
                        return Ok(false);
                    }
                    tokio::time::sleep(poll_interval).await;
                }
                Err(FastbootError::Transport(TransportError::Timeout)) => {
                    if start.elapsed() >= max_duration {
                        self.set_timeout(original_timeout);
                        return Ok(false);
                    }
                    tokio::time::sleep(poll_interval).await;
                }
                Err(e) => {
                    self.set_timeout(original_timeout);
                    return Err(e);
                }
            }
        }
    }

    pub async fn sync_device(&mut self) -> Result<(), FastbootError> {
        let _ = self.reset().await;

        tokio::time::sleep(Duration::from_millis(100)).await;

        let ready = self.wait_for_device_ready(30, 500).await?;

        if !ready {
            tokio::time::sleep(Duration::from_secs(2)).await;
        }

        Ok(())
    }

    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
    }

    pub async fn get_var(&mut self, key: &str) -> Result<String, FastbootError> {
        let cmd = format!("{}:{}", protocol::FB_CMD_GETVAR, key);
        let response = self.raw_command(&cmd).await?;

        match response {
            Response::Okay(val) => Ok(val),
            Response::Fail(msg) => Err(FastbootError::Device(msg)),
            _ => Err(FastbootError::Protocol("Unexpected response".into())),
        }
    }

    pub async fn get_var_all(&mut self) -> Result<Vec<String>, FastbootError> {
        let cmd = format!("{}:all", protocol::FB_CMD_GETVAR);
        let (_, info_messages) = self.raw_command_with_info(&cmd).await?;
        Ok(info_messages)
    }

    pub async fn get_max_download_size(&mut self) -> Result<u64, FastbootError> {
        if let Some(size) = self.max_download_size {
            return Ok(size);
        }

        let size_str = self.get_var("max-download-size").await?;
        let size = parse_size(&size_str)?;
        self.max_download_size = Some(size);
        Ok(size)
    }

    pub async fn download(&mut self, data: &[u8]) -> Result<(), FastbootError> {
        if !self.disable_checks && data.is_empty() {
            return Err(FastbootError::InvalidArg("Data is empty".into()));
        }

        let cmd = format!("{}:{:08x}", protocol::FB_CMD_DOWNLOAD, data.len());
        let response = self.raw_command(&cmd).await?;

        let expected_size = match response {
            Response::Data(size) => size as usize,
            Response::Fail(msg) => return Err(FastbootError::Device(msg)),
            _ => return Err(FastbootError::Protocol("Expected DATA response".into())),
        };

        if expected_size != data.len() {
            return Err(FastbootError::Protocol(format!(
                "Size mismatch: expected {}, got {}",
                data.len(),
                expected_size
            )));
        }

        self.send_data(data).await?;

        let final_response = self.handle_response().await?;
        match final_response {
            Response::Okay(_) => Ok(()),
            Response::Fail(msg) => Err(FastbootError::Device(msg)),
            _ => Err(FastbootError::Protocol("Expected OKAY after download".into())),
        }
    }

    pub async fn download_fast(&mut self, data: &[u8]) -> Result<(), FastbootError> {
        let cmd = format!("{}:{:08x}", protocol::FB_CMD_DOWNLOAD, data.len());

        self.transport
            .write_all(cmd.as_bytes())
            .await
            .map_err(FastbootError::Transport)?;

        let response = self.handle_response().await?;
        match response {
            Response::Data(_) => {}
            Response::Fail(msg) => return Err(FastbootError::Device(msg)),
            _ => return Err(FastbootError::Protocol("Expected DATA response".into())),
        }

        self.transport
            .write_all(data)
            .await
            .map_err(FastbootError::Transport)?;

        let final_response = self.handle_response().await?;
        match final_response {
            Response::Okay(_) => Ok(()),
            Response::Fail(msg) => Err(FastbootError::Device(msg)),
            _ => Err(FastbootError::Protocol("Expected OKAY after download".into())),
        }
    }

    pub async fn download_file(&mut self, path: &Path) -> Result<(), FastbootError> {
        let file = File::open(path).await.map_err(FastbootError::Io)?;
        let metadata = file.metadata().await.map_err(FastbootError::Io)?;
        let file_size = metadata.len();

        let cmd = format!("{}:{:08x}", protocol::FB_CMD_DOWNLOAD, file_size);
        let response = self.raw_command(&cmd).await?;

        match response {
            Response::Data(size) => {
                if size as u64 != file_size {
                    return Err(FastbootError::Protocol(format!(
                        "Size mismatch: expected {}, got {}",
                        file_size, size
                    )));
                }
            }
            Response::Fail(msg) => return Err(FastbootError::Device(msg)),
            _ => return Err(FastbootError::Protocol("Expected DATA response".into())),
        }

        self.send_file_data(file, file_size).await?;

        let final_response = self.handle_response().await?;
        match final_response {
            Response::Okay(_) => Ok(()),
            Response::Fail(msg) => Err(FastbootError::Device(msg)),
            _ => Err(FastbootError::Protocol("Expected OKAY after download".into())),
        }
    }

    pub async fn upload(&mut self) -> Result<Vec<u8>, FastbootError> {
        let response = self.raw_command(protocol::FB_CMD_UPLOAD).await?;

        let size = match response {
            Response::Data(size) => size as usize,
            Response::Fail(msg) => return Err(FastbootError::Device(msg)),
            _ => return Err(FastbootError::Protocol("Expected DATA response".into())),
        };

        let data = self.recv_data(size).await?;

        let final_response = self.handle_response().await?;
        match final_response {
            Response::Okay(_) => Ok(data),
            Response::Fail(msg) => Err(FastbootError::Device(msg)),
            _ => Err(FastbootError::Protocol("Expected OKAY after upload".into())),
        }
    }

    pub async fn upload_to_file(&mut self, path: &Path) -> Result<u64, FastbootError> {
        let response = self.raw_command(protocol::FB_CMD_UPLOAD).await?;

        let size = match response {
            Response::Data(size) => size as u64,
            Response::Fail(msg) => return Err(FastbootError::Device(msg)),
            _ => return Err(FastbootError::Protocol("Expected DATA response".into())),
        };

        let mut file = File::create(path).await.map_err(FastbootError::Io)?;

        let received = self.recv_data_to_file(&mut file, size).await?;

        let final_response = self.handle_response().await?;
        match final_response {
            Response::Okay(_) => Ok(received),
            Response::Fail(msg) => Err(FastbootError::Device(msg)),
            _ => Err(FastbootError::Protocol("Expected OKAY after upload".into())),
        }
    }

    pub async fn read_partition(&mut self, partition: &str, path: &Path) -> Result<u64, FastbootError> {
        let cmd = format!("{}:{}", protocol::FB_CMD_FETCH, partition);
        let response = self.raw_command(&cmd).await?;

        match response {
            Response::Okay(_) => {}
            Response::Fail(msg) => return Err(FastbootError::Device(msg)),
            _ => return Err(FastbootError::Protocol("Expected OKAY for fetch".into())),
        }

        self.upload_to_file(path).await
    }

    pub async fn flash(&mut self, partition: &str) -> Result<(), FastbootError> {
        let cmd = format!("{}:{}", protocol::FB_CMD_FLASH, partition);
        (self.callbacks.prolog)(&format!("Writing '{}'", partition));

        let response = self.raw_command(&cmd).await?;
        match response {
            Response::Okay(_) => {
                (self.callbacks.epilog)(RetCode::Success);
                Ok(())
            }
            Response::Fail(msg) => {
                (self.callbacks.epilog)(RetCode::DeviceFail);
                Err(FastbootError::Device(msg))
            }
            _ => {
                (self.callbacks.epilog)(RetCode::BadDeviceResponse);
                Err(FastbootError::Protocol("Unexpected response".into()))
            }
        }
    }

    pub async fn flash_fast(&mut self, partition: &str) -> Result<(), FastbootError> {
        let cmd = format!("{}:{}", protocol::FB_CMD_FLASH, partition);

        self.transport
            .write_all(cmd.as_bytes())
            .await
            .map_err(FastbootError::Transport)?;

        let response = self.handle_response().await?;
        match response {
            Response::Okay(_) => Ok(()),
            Response::Fail(msg) => Err(FastbootError::Device(msg)),
            _ => Err(FastbootError::Protocol("Unexpected response".into())),
        }
    }

    pub async fn flash_partition(&mut self, partition: &str, data: &[u8]) -> Result<(), FastbootError> {
        self.download(data).await?;
        self.flash(partition).await
    }

    pub async fn flash_partition_file(&mut self, partition: &str, path: &Path) -> Result<(), FastbootError> {
        self.download_file(path).await?;
        self.flash(partition).await
    }

    pub async fn erase(&mut self, partition: &str) -> Result<(), FastbootError> {
        let cmd = format!("{}:{}", protocol::FB_CMD_ERASE, partition);
        (self.callbacks.prolog)(&format!("Erasing '{}'", partition));

        let response = self.raw_command(&cmd).await?;
        match response {
            Response::Okay(_) => {
                (self.callbacks.epilog)(RetCode::Success);
                Ok(())
            }
            Response::Fail(msg) => {
                (self.callbacks.epilog)(RetCode::DeviceFail);
                Err(FastbootError::Device(msg))
            }
            _ => {
                (self.callbacks.epilog)(RetCode::BadDeviceResponse);
                Err(FastbootError::Protocol("Unexpected response".into()))
            }
        }
    }

    pub async fn boot(&mut self) -> Result<(), FastbootError> {
        let response = self.raw_command(protocol::FB_CMD_BOOT).await?;
        match response {
            Response::Okay(_) => Ok(()),
            Response::Fail(msg) => Err(FastbootError::Device(msg)),
            _ => Err(FastbootError::Protocol("Unexpected response".into())),
        }
    }

    pub async fn continue_boot(&mut self) -> Result<(), FastbootError> {
        let response = self.raw_command(protocol::FB_CMD_CONTINUE).await?;
        match response {
            Response::Okay(_) => Ok(()),
            Response::Fail(msg) => Err(FastbootError::Device(msg)),
            _ => Err(FastbootError::Protocol("Unexpected response".into())),
        }
    }

    pub async fn reboot(&mut self) -> Result<(), FastbootError> {
        let response = self.raw_command(protocol::FB_CMD_REBOOT).await?;
        match response {
            Response::Okay(_) => Ok(()),
            Response::Fail(msg) => Err(FastbootError::Device(msg)),
            _ => Err(FastbootError::Protocol("Unexpected response".into())),
        }
    }

    pub async fn reboot_to(&mut self, target: &str) -> Result<(), FastbootError> {
        let cmd = format!("reboot-{}", target);
        let response = self.raw_command(&cmd).await?;
        match response {
            Response::Okay(_) => Ok(()),
            Response::Fail(msg) => Err(FastbootError::Device(msg)),
            _ => Err(FastbootError::Protocol("Unexpected response".into())),
        }
    }

    pub async fn set_active(&mut self, slot: &str) -> Result<(), FastbootError> {
        let cmd = format!("{}:{}", protocol::FB_CMD_SET_ACTIVE, slot);
        let response = self.raw_command(&cmd).await?;
        match response {
            Response::Okay(_) => Ok(()),
            Response::Fail(msg) => Err(FastbootError::Device(msg)),
            _ => Err(FastbootError::Protocol("Unexpected response".into())),
        }
    }

    pub async fn create_partition(&mut self, name: &str, size: u64) -> Result<(), FastbootError> {
        let cmd = format!("{}:{}:{}", protocol::FB_CMD_CREATE_PARTITION, name, size);
        let response = self.raw_command(&cmd).await?;
        match response {
            Response::Okay(_) => Ok(()),
            Response::Fail(msg) => Err(FastbootError::Device(msg)),
            _ => Err(FastbootError::Protocol("Unexpected response".into())),
        }
    }

    pub async fn delete_partition(&mut self, name: &str) -> Result<(), FastbootError> {
        let cmd = format!("{}:{}", protocol::FB_CMD_DELETE_PARTITION, name);
        let response = self.raw_command(&cmd).await?;
        match response {
            Response::Okay(_) => Ok(()),
            Response::Fail(msg) => Err(FastbootError::Device(msg)),
            _ => Err(FastbootError::Protocol("Unexpected response".into())),
        }
    }

    pub async fn resize_partition(&mut self, name: &str, size: u64) -> Result<(), FastbootError> {
        let cmd = format!("{}:{}:{}", protocol::FB_CMD_RESIZE_PARTITION, name, size);
        let response = self.raw_command(&cmd).await?;
        match response {
            Response::Okay(_) => Ok(()),
            Response::Fail(msg) => Err(FastbootError::Device(msg)),
            _ => Err(FastbootError::Protocol("Unexpected response".into())),
        }
    }

    pub async fn oem_command(&mut self, cmd: &str) -> Result<String, FastbootError> {
        let full_cmd = format!("{}:{}", protocol::FB_CMD_OEM, cmd);
        let response = self.raw_command(&full_cmd).await?;
        match response {
            Response::Okay(msg) => Ok(msg),
            Response::Fail(msg) => Err(FastbootError::Device(msg)),
            _ => Err(FastbootError::Protocol("Unexpected response".into())),
        }
    }

    pub async fn raw_command(&mut self, cmd: &str) -> Result<Response, FastbootError> {
        let (response, _) = self.raw_command_with_info(cmd).await?;
        Ok(response)
    }

    pub async fn raw_command_with_info(
        &mut self,
        cmd: &str,
    ) -> Result<(Response, Vec<String>), FastbootError> {
        self.last_error.clear();

        if !self.disable_checks && cmd.len() > FB_COMMAND_SZ {
            return Err(FastbootError::InvalidArg(format!(
                "Command too long: {} bytes (max {})",
                cmd.len(),
                FB_COMMAND_SZ
            )));
        }

        let cmd_owned = cmd.to_string();
        let max_attempts = if self.auto_retry { self.max_retries } else { 1 };
        let mut last_error = None;

        for attempt in 0..max_attempts {
            match self.send_command_internal(&cmd_owned).await {
                Ok(result) => {
                    self.consecutive_failures = 0;
                    return Ok(result);
                }
                Err(e) => {
                    self.consecutive_failures += 1;

                    if !e.is_recoverable() || attempt + 1 >= max_attempts {
                        return Err(e);
                    }

                    self.last_error = format!("尝试 {}/{} 失败: {}", attempt + 1, max_attempts, e);
                    last_error = Some(e);

                    if self.try_recover().await.is_err() {
                        return Err(last_error.unwrap());
                    }

                    let delay = RETRY_DELAY_MS * (1 << attempt);
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                }
            }
        }

        Err(last_error.unwrap_or(FastbootError::Timeout))
    }

    async fn send_command_internal(
        &mut self,
        cmd: &str,
    ) -> Result<(Response, Vec<String>), FastbootError> {
        self.transport
            .write_all(cmd.as_bytes())
            .await
            .map_err(|e| {
                match &e {
                    TransportError::Usb(msg) => FastbootError::Transport(TransportError::from_usb_error(msg)),
                    _ => FastbootError::Transport(e),
                }
            })?;

        let mut info_messages = Vec::new();

        loop {
            let response = self.handle_response().await?;

            match &response {
                Response::Info(msg) => {
                    (self.callbacks.info)(msg);
                    info_messages.push(msg.clone());
                }
                Response::Text(msg) => {
                    (self.callbacks.text)(msg);
                }
                _ => {
                    return Ok((response, info_messages));
                }
            }
        }
    }

    async fn handle_response(&mut self) -> Result<Response, FastbootError> {
        let mut buf = [0u8; FB_RESPONSE_SZ + 1];

        let n = self
            .transport
            .read_with_timeout(&mut buf, self.timeout)
            .await
            .map_err(|e| {
                match &e {
                    TransportError::Timeout => FastbootError::Timeout,
                    TransportError::Disconnected | TransportError::NoLink => {
                        FastbootError::Transport(TransportError::NoLink)
                    }
                    TransportError::Usb(msg) => {
                        FastbootError::Transport(TransportError::from_usb_error(msg))
                    }
                    _ => FastbootError::Transport(e),
                }
            })?;

        if n == 0 {
            return Err(FastbootError::Transport(TransportError::NoLink));
        }

        let (response, _) = Response::parse(&buf[..n])
            .map_err(|e| FastbootError::Protocol(e.to_string()))?;

        Ok(response)
    }

    async fn send_data(&mut self, data: &[u8]) -> Result<(), FastbootError> {
        let total = data.len() as u64;
        let mut sent = 0u64;

        for chunk in data.chunks(DOWNLOAD_CHUNK_SIZE) {
            self.transport
                .write_all(chunk)
                .await
                .map_err(FastbootError::Transport)?;

            sent += chunk.len() as u64;

            if let Some(ref progress) = self.callbacks.progress {
                progress(sent, total);
            }
        }

        Ok(())
    }

    async fn send_file_data(&mut self, mut file: File, total: u64) -> Result<(), FastbootError> {
        let mut buf = vec![0u8; DOWNLOAD_CHUNK_SIZE];
        let mut sent = 0u64;

        loop {
            let n = file.read(&mut buf).await.map_err(FastbootError::Io)?;
            if n == 0 {
                break;
            }

            self.transport
                .write_all(&buf[..n])
                .await
                .map_err(FastbootError::Transport)?;

            sent += n as u64;

            if let Some(ref progress) = self.callbacks.progress {
                progress(sent, total);
            }
        }

        Ok(())
    }

    async fn recv_data(&mut self, size: usize) -> Result<Vec<u8>, FastbootError> {
        let mut data = vec![0u8; size];
        let mut received = 0;

        while received < size {
            let n = self
                .transport
                .read(&mut data[received..])
                .await
                .map_err(FastbootError::Transport)?;

            if n == 0 {
                return Err(FastbootError::Transport(TransportError::Disconnected));
            }

            received += n;

            if let Some(ref progress) = self.callbacks.progress {
                progress(received as u64, size as u64);
            }
        }

        Ok(data)
    }

    async fn recv_data_to_file(&mut self, file: &mut File, total: u64) -> Result<u64, FastbootError> {
        let mut buf = vec![0u8; UPLOAD_CHUNK_SIZE];
        let mut received = 0u64;

        while received < total {
            let to_read = std::cmp::min(buf.len() as u64, total - received) as usize;
            let n = self
                .transport
                .read(&mut buf[..to_read])
                .await
                .map_err(FastbootError::Transport)?;

            if n == 0 {
                return Err(FastbootError::Transport(TransportError::Disconnected));
            }

            file.write_all(&buf[..n]).await.map_err(FastbootError::Io)?;
            received += n as u64;

            if let Some(ref progress) = self.callbacks.progress {
                progress(received, total);
            }
        }

        file.flush().await.map_err(FastbootError::Io)?;
        Ok(received)
    }

    pub fn last_error(&self) -> &str {
        &self.last_error
    }
}

fn parse_size(s: &str) -> Result<u64, FastbootError> {
    let s = s.trim();
    if s.starts_with("0x") || s.starts_with("0X") {
        u64::from_str_radix(&s[2..], 16)
    } else {
        s.parse()
    }
    .map_err(|_| FastbootError::Protocol(format!("Invalid size: {}", s)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_size() {
        assert_eq!(parse_size("1234").unwrap(), 1234);
        assert_eq!(parse_size("0x1234").unwrap(), 0x1234);
        assert_eq!(parse_size("0X1234").unwrap(), 0x1234);
        assert_eq!(parse_size("  1234  ").unwrap(), 1234);
        assert!(parse_size("invalid").is_err());
    }
}