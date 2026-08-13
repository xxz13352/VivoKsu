use std::path::Path;
use std::time::Duration;

use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::error::{FastbootError, RetCode, TransportError};
use crate::protocol::{self, Response, FB_COMMAND_SZ, FB_RESPONSE_SZ};
use crate::transport::{AsyncTransport, AsyncTransportExt};
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// flash 擦写阶段的独立长超时：super 等大分区擦写期间设备可能长时间不回包，
/// 用 30s 常规超时会误判超时并打断正在进行的刷写。600s 远大于单次 flash 实际耗时，足够安全。
const FLASH_TIMEOUT: Duration = Duration::from_secs(600);
const DOWNLOAD_CHUNK_SIZE: usize = 1 * 1024 * 1024;
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
    flash_timeout: Duration,
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
            flash_timeout: FLASH_TIMEOUT,
            last_error: String::new(),
            max_download_size: None,
            auto_retry: true,
            max_retries: MAX_RETRY_COUNT,
            consecutive_failures: 0,
        }
    }
    /// 消费 driver，取回底层 transport。
    /// 用于 open 阶段：用临时 driver 探测句柄能否真正 IO 后，把可用连接交还调用方。
    pub fn into_transport(self) -> T {
        self.transport
    }
    pub fn set_callbacks(&mut self, callbacks: DriverCallbacks) {
        self.callbacks = callbacks;
    }
    pub fn set_progress_callback(&mut self, callback: ProgressCallback) {
        self.callbacks.progress = Some(callback);
    }
    /// 设置 INFO 回调（设备在 flash/erase 等阶段回传的 INFO 心跳，含进度文本时调用方可据此推进进度）。
    /// 单独设置，不影响已设置的 progress 回调。
    pub fn set_info_callback(&mut self, callback: Box<dyn Fn(&str) + Send + Sync>) {
        self.callbacks.info = callback;
    }
    /// 设置 prolog 回调（命令开始时触发，如 "Writing 'super'" / "Erasing 'userdata'"）。
    pub fn set_prolog_callback(&mut self, callback: Box<dyn Fn(&str) + Send + Sync>) {
        self.callbacks.prolog = callback;
    }
    pub fn disable_checks(&mut self) {
        self.disable_checks = true;
    }
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
        self.transport.set_timeout(timeout);
    }
    /// 设置 flash 擦写阶段的独立超时（默认 600s）。用于覆盖大分区超长擦写场景。
    pub fn set_flash_timeout(&mut self, timeout: Duration) {
        self.flash_timeout = timeout;
    }
    pub fn set_auto_retry(&mut self, enabled: bool) {
        self.auto_retry = enabled;
    }
    pub fn set_max_retries(&mut self, count: u32) {
        self.max_retries = count;
    }

    pub async fn reset(&mut self) -> Result<(), FastbootError> {
        self.transport
            .reset()
            .await
            .map_err(FastbootError::Transport)
    }

    pub async fn deep_reset(&mut self) -> Result<(), FastbootError> {
        self.transport
            .reinitialize()
            .await
            .map_err(FastbootError::Transport)
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
        match tokio::time::timeout(Duration::from_secs(2), self.transport.read(&mut buf)).await {
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
                    tokio::time::sleep(Duration::from_millis(
                        RETRY_DELAY_MS * (attempt as u64 + 1),
                    ))
                    .await;
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

        let size = match self.get_var("max-download-size").await {
            Ok(size_str) => parse_size(&size_str).unwrap_or(256 * 1024 * 1024),
            Err(_) => 256 * 1024 * 1024,
        };
        self.max_download_size = Some(size);
        Ok(size)
    }

    pub async fn download(&mut self, data: &[u8]) -> Result<(), FastbootError> {
        if !self.disable_checks && data.is_empty() {
            return Err(FastbootError::InvalidArg("待下载数据为空".into()));
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
            _ => Err(FastbootError::Protocol(
                "Expected OKAY after download".into(),
            )),
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
            _ => Err(FastbootError::Protocol(
                "Expected OKAY after download".into(),
            )),
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
            _ => Err(FastbootError::Protocol(
                "Expected OKAY after download".into(),
            )),
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

    pub async fn read_partition(
        &mut self,
        partition: &str,
        path: &Path,
    ) -> Result<u64, FastbootError> {
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

        // flash 阶段设备要擦除+写入，super 等大分区常需数十秒到数分钟，期间可能完全不回包。
        // 1) 用独立长超时(flash_timeout)代替 30s 常规超时，避免把正在进行的擦写误判为超时。
        // 2) 直接走 send_command_internal(不带重试)：flash 命令一旦发出绝不能因超时而重发——
        //    重发会打断设备正在进行的擦写，正是"连续刷写卡顿/打断"的根因。
        // send_command_internal 内部循环读响应时会把设备的 INFO 心跳转发给 callbacks.info，
        // 调用方据此可在 flash 期间持续更新进度，不再卡在 100%。
        let prev_timeout = self.timeout;
        let flash_timeout = self.flash_timeout.max(prev_timeout);
        self.set_timeout(flash_timeout);

        let result = self.send_command_internal(&cmd).await;

        self.set_timeout(prev_timeout);

        let (response, _info) = result?;
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
    pub async fn flash_partition(
        &mut self,
        partition: &str,
        data: &[u8],
    ) -> Result<(), FastbootError> {
        self.download(data).await?;
        self.flash(partition).await
    }
    pub async fn flash_partition_file(
        &mut self,
        partition: &str,
        path: &Path,
    ) -> Result<(), FastbootError> {
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
        self.send_reboot_command(protocol::FB_CMD_REBOOT.to_string())
            .await
    }
    pub async fn reboot_to(&mut self, target: &str) -> Result<(), FastbootError> {
        self.send_reboot_command(format!("reboot-{}", target)).await
    }
    /// 发送 reboot 类命令。语义特殊：命令一旦被设备接受，设备会立即重启导致 USB 连接断开，
    /// 此时读响应会得到「传输断开/超时」——这属于「重启已生效」的正常结果，按成功处理。
    /// 只有设备明确回 FAIL（拒绝/不支持该目标，例如 fastbootd 不支持 reboot-edl）才算失败。
    /// reboot 后重试无意义（设备已离开），故强制单发不重试，避免把成功重启误判后反复重发。
    async fn send_reboot_command(&mut self, cmd: String) -> Result<(), FastbootError> {
        let prev_retry = self.auto_retry;
        self.set_auto_retry(false);
        let result = self.raw_command(&cmd).await;
        self.set_auto_retry(prev_retry);
        match result {
            Ok(Response::Okay(_)) => Ok(()),
            Ok(Response::Fail(msg)) => Err(FastbootError::Device(msg)),
            Ok(_) => Err(FastbootError::Protocol("Unexpected response".into())),
            // 设备接受 reboot 后立即重启 → 连接断开/超时，是预期的成功信号
            Err(FastbootError::Transport(_)) | Err(FastbootError::Timeout) => Ok(()),
            Err(e) => Err(e),
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
        // 标准 fastboot：`fastboot oem <args>` 在线缆上发送 "oem <args>"（空格分隔）。
        // 此前用冒号拼成 "oem:edl"，高通等 bootloader 解析 OEM 子命令按空格分词，
        // 冒号形式会被判为未知命令直接返回 FAIL，导致真机上 oem edl/unlock 等全部失效。
        let full_cmd = format!("{} {}", protocol::FB_CMD_OEM, cmd);
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
            .map_err(|e| match &e {
                TransportError::Usb(msg) => {
                    FastbootError::Transport(TransportError::from_usb_error(msg))
                }
                _ => FastbootError::Transport(e),
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
            .map_err(|e| match &e {
                TransportError::Timeout => FastbootError::Timeout,
                TransportError::Disconnected | TransportError::NoLink => {
                    FastbootError::Transport(TransportError::NoLink)
                }
                TransportError::Usb(msg) => {
                    FastbootError::Transport(TransportError::from_usb_error(msg))
                }
                _ => FastbootError::Transport(e),
            })?;

        if n == 0 {
            return Err(FastbootError::Transport(TransportError::NoLink));
        }

        let (response, _) =
            Response::parse(&buf[..n]).map_err(|e| FastbootError::Protocol(e.to_string()))?;

        Ok(response)
    }
    async fn send_data(&mut self, data: &[u8]) -> Result<(), FastbootError> {
        let total = data.len() as u64;

        for chunk in data.chunks(DOWNLOAD_CHUNK_SIZE) {
            self.transport
                .write_all(chunk)
                .await
                .map_err(FastbootError::Transport)?;

            if let Some(ref progress) = self.callbacks.progress {
                progress(chunk.len() as u64, total);
            }
        }

        Ok(())
    }
    async fn send_file_data(&mut self, mut file: File, total: u64) -> Result<(), FastbootError> {
        let mut buf = vec![0u8; DOWNLOAD_CHUNK_SIZE];

        loop {
            let n = file.read(&mut buf).await.map_err(FastbootError::Io)?;
            if n == 0 {
                break;
            }

            self.transport
                .write_all(&buf[..n])
                .await
                .map_err(FastbootError::Transport)?;

            if let Some(ref progress) = self.callbacks.progress {
                progress(n as u64, total);
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
    async fn recv_data_to_file(
        &mut self,
        file: &mut File,
        total: u64,
    ) -> Result<u64, FastbootError> {
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
