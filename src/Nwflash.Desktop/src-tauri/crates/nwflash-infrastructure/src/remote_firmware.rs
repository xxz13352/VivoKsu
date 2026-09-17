//! 远端固件读取：通过 HTTP Range 按需读取并不下载整包。
//!
//! 用于 Vivo ROOT「云端 OTA 提取」：服务器解析出 OTA 链接后，客户端只拉取修补所需
//! 的启动分区镜像（init_boot / boot / vendor_boot），而非整个 OTA（可达 5–9 GB）。
//!
//! - `probe_remote_kind` 用首字节魔数识别 payload OTA / 直接镜像 zip / 裸 payload。
//! - `RangeHttpReader` 提供 `Read + Seek`，供 `zip` crate 直接读取远程 zip 的中央目录
//!   （只有几十 KB）并定向解压少量成员。
//! - `extract_zip_members` 只下载并解压目标成员的字节，CRC/长度由 zip crate 校验。

use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::Duration;

use reqwest::blocking::Client;
use reqwest::header::{CONTENT_RANGE, RANGE};
use reqwest::StatusCode;
use sha2::{Digest as _, Sha256};
use thiserror::Error;
use url::Url;
use zip::ZipArchive;

/// 每次网络拉取的填充块上限（字节）。非 0 保证取消检查按块触发。
const CHUNK_BYTES: u64 = 1024 * 1024;

/// 建连超时。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// 单次网络操作超时。阻塞 reqwest 把它施加到**每一次** `Read::read` 上，
/// 所以语义是「这么久收不到新字节就中断本次读取」，而不是整个响应的总时限——
/// 慢速但持续有数据的传输不会被误杀。
const STALL_TIMEOUT: Duration = Duration::from_secs(60);
/// 单次 Range 请求的目标窗口。Vivo 固件 CDN（火山引擎 TOS 源站 + 网宿/百度
/// 多级 CDN）会在长 Range 响应中途断流（reqwest 报 `error decoding response
/// body`）：窗口越小，断流损失越小，重试从已收字节续传。
const RANGE_SUBREQUEST_BYTES: u64 = 4 * 1024 * 1024;
/// 单个窗口允许的「零进展」重试次数；一旦收到字节就重置计数。
const RANGE_MAX_ATTEMPTS_WITHOUT_PROGRESS: u8 = 5;
/// 单次窗口拉取的请求次数上限（服务器每次只回极少字节时的失控保护）。
const RANGE_MAX_REQUESTS: u64 = 4096;
/// 重试退避基数，按次数指数放大。
const RANGE_RETRY_BACKOFF: Duration = Duration::from_millis(400);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteFirmwareKind {
    /// 远程 URL 是 zip 且含 payload.bin（payload OTA）。
    PayloadZip,
    /// 远程 URL 直接是 payload.bin（CrAU 魔数）。
    PayloadRaw,
    /// 远程 URL 是 zip 但不含 payload.bin（直接镜像 / 块式 OTA）。
    DirectImageZip,
    /// 其它格式（gzip / tar / 未知）。
    Unsupported,
}

#[derive(Debug, Error)]
pub enum RemoteFirmwareError {
    #[error("固件地址不能为空。")]
    InvalidUrl(String),
    #[error("读取远程固件失败：{0}")]
    Transport(String),
    #[error("远程固件服务器不支持 Range 请求。")]
    RangeUnsupported,
    #[error("不支持的固件格式。")]
    UnsupportedFormat,
    #[error("读取固件压缩包失败：{0}")]
    Archive(String),
    #[error("固件包中不存在分区 {0}。")]
    MissingPartition(String),
    #[error("固件分区完整性校验失败：{0}")]
    Integrity(String),
    #[error("提取已取消。")]
    Cancelled,
}

#[derive(Debug, Clone)]
pub struct ZipMember {
    pub name: String,
    /// zip 内完整路径（含目录，用于 by_name 解压）。
    pub full_name: String,
    pub size_bytes: i64,
}

#[derive(Debug, Clone)]
pub struct ExtractedZipImage {
    pub partition_name: String,
    pub output_path: String,
    pub size_bytes: i64,
}

fn default_client() -> Client {
    Client::builder()
        .user_agent("Nwflash/1.0.1")
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(STALL_TIMEOUT)
        .build()
        .expect("reqwest blocking client should build")
}

/// 把 reqwest 错误的完整原因链渲染进用户可见文案。`Display` 只给
/// `error decoding response body` 这类顶层描述，底层 hyper/rustls/IO 原因藏在
/// `source()` 里——不展开就无法区分「连接被重置」「响应体提前结束」
/// 「TLS 未发 close_notify」等完全不同的故障。
fn describe_reqwest_error(error: &reqwest::Error) -> String {
    describe_error_chain(error)
}

fn describe_io_error(error: &io::Error) -> String {
    describe_error_chain(error)
}

/// 供同 crate 的其它网络路径（如 `firmware_extract` 的远程探测）复用。
pub(crate) fn describe_error_chain(error: &dyn std::error::Error) -> String {
    let mut description = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        description.push_str(" ← ");
        description.push_str(&cause.to_string());
        source = cause.source();
    }
    description
}

fn is_canceled_or<F: FnMut() -> bool>(is_canceled: &mut F) -> Result<(), RemoteFirmwareError> {
    if is_canceled() {
        Err(RemoteFirmwareError::Cancelled)
    } else {
        Ok(())
    }
}

pub fn validate_http_url(url: &str) -> Result<(), RemoteFirmwareError> {
    let parsed = Url::parse(url.trim()).map_err(|_| {
        RemoteFirmwareError::InvalidUrl("固件地址必须是有效的 HTTP 或 HTTPS URL。".to_string())
    })?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(RemoteFirmwareError::InvalidUrl(
            "固件地址必须使用 HTTP 或 HTTPS。".to_string(),
        ));
    }
    Ok(())
}

fn validate_url(url: &str) -> Result<(), RemoteFirmwareError> {
    validate_http_url(url)
}

/// 探测远程 OTA 的格式（按需 Range 读取首字节）。
pub fn probe_remote_kind<F>(
    url: &str,
    client: Option<&Client>,
    is_canceled: &mut F,
) -> Result<RemoteFirmwareKind, RemoteFirmwareError>
where
    F: FnMut() -> bool,
{
    validate_url(url)?;
    is_canceled_or(is_canceled)?;
    let body = fetch_range(url, client, 0, 3, is_canceled)?;
    if body.len() >= 4 && &body[0..4] == b"CrAU" {
        return Ok(RemoteFirmwareKind::PayloadRaw);
    }
    if body.len() >= 2 && &body[0..2] == b"PK" {
        // zip：查中央目录是否含 payload.bin。
        let members = list_zip_members(url, client, is_canceled)?;
        let has_payload = members
            .iter()
            .any(|member| member.name == "payload" && !member.full_name.is_empty());
        return Ok(if has_payload {
            RemoteFirmwareKind::PayloadZip
        } else {
            RemoteFirmwareKind::DirectImageZip
        });
    }
    Ok(RemoteFirmwareKind::Unsupported)
}

/// 列出远程 zip 的成员（只拉取中央目录）。取消检查由内部 reader 的每次网络读取触发。
pub fn list_zip_members<F>(
    url: &str,
    client: Option<&Client>,
    is_canceled: &mut F,
) -> Result<Vec<ZipMember>, RemoteFirmwareError>
where
    F: FnMut() -> bool,
{
    validate_url(url)?;
    let reader = RangeHttpReader::new(url, client, is_canceled)?;
    let mut archive =
        ZipArchive::new(reader).map_err(|error| RemoteFirmwareError::Archive(error.to_string()))?;
    let mut members = Vec::with_capacity(archive.len());
    for index in 0..archive.len() {
        let file = archive.by_index(index).map_err(|error| {
            RemoteFirmwareError::Archive(format!("读取压缩包入口失败：{error}"))
        })?;
        let full_name = file.name().to_string();
        let base = Path::new(&full_name)
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .unwrap_or_default();
        let size = file.size() as i64;
        let member = ZipMember {
            name: strip_extension(&base),
            full_name: full_name.clone(),
            size_bytes: size,
        };
        if !full_name.ends_with('/') {
            members.push(member);
        }
    }
    Ok(members)
}

fn strip_extension(name: &str) -> String {
    Path::new(name)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(name)
        .to_string()
}

/// 定向解压远程 zip 中目标分区成员到 `output_dir`。仅下载所需的成员字节。
pub fn extract_zip_members<F, P>(
    url: &str,
    client: Option<&Client>,
    wanted: &[&str],
    output_dir: &Path,
    is_canceled: &mut F,
    report_progress: &mut P,
) -> Result<Vec<ExtractedZipImage>, RemoteFirmwareError>
where
    F: FnMut() -> bool,
    P: FnMut(&str, u64),
{
    validate_url(url)?;
    let reader = RangeHttpReader::new(url, client, is_canceled)?;
    let mut archive =
        ZipArchive::new(reader).map_err(|error| RemoteFirmwareError::Archive(error.to_string()))?;

    std::fs::create_dir_all(output_dir)
        .map_err(|error| RemoteFirmwareError::Transport(format!("创建提取目录失败：{error}")))?;

    let mut seen = std::collections::HashSet::new();
    let mut candidates = Vec::new();
    for index in 0..archive.len() {
        let file = archive.by_index(index).map_err(|error| {
            RemoteFirmwareError::Archive(format!("读取压缩包入口失败：{error}"))
        })?;
        let full_name = file.name().to_string();
        if full_name.ends_with('/') {
            continue;
        }
        let base = Path::new(&full_name)
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .unwrap_or("")
            .to_string();
        let partition_name = strip_extension(&base);
        if !wanted.contains(&partition_name.as_str()) {
            continue;
        }
        if !seen.insert(partition_name.clone()) {
            continue;
        }
        candidates.push((index, partition_name, file.size()));
    }

    let mut results = Vec::new();
    for (index, partition_name, expected_size) in candidates {
        let mut entry = archive.by_index(index).map_err(|error| {
            RemoteFirmwareError::Archive(format!("读取压缩包入口失败：{error}"))
        })?;

        let output_path = output_dir.join(format!("{partition_name}.img"));
        let partial = output_path.with_file_name(format!(".{partition_name}.partial"));
        let partial_result = (|| -> Result<(), RemoteFirmwareError> {
            let mut output = std::fs::File::create(&partial).map_err(|error| {
                RemoteFirmwareError::Transport(format!("创建提取文件失败：{error}"))
            })?;
            let mut buffer = [0u8; 64 * 1024];
            let mut written: u64 = 0;
            loop {
                // 取消检查由 reader 的每次网络读取触发（每块最多 CHUNK_BYTES）。
                let count = entry.read(&mut buffer).map_err(|error| {
                    if error.kind() == io::ErrorKind::Interrupted {
                        RemoteFirmwareError::Cancelled
                    } else {
                        RemoteFirmwareError::Archive(format!(
                            "解压分区 {partition_name} 失败：{error}"
                        ))
                    }
                })?;
                if count == 0 {
                    break;
                }
                output.write_all(&buffer[..count]).map_err(|error| {
                    RemoteFirmwareError::Transport(format!(
                        "写入分区 {partition_name} 失败：{error}"
                    ))
                })?;
                written = written.saturating_add(count as u64);
                report_progress(&partition_name, written);
            }
            output.flush().map_err(|error| {
                RemoteFirmwareError::Transport(format!("写入分区 {partition_name} 失败：{error}"))
            })?;
            Ok(())
        })();
        if let Err(error) = partial_result {
            let _ = std::fs::remove_file(&partial);
            return Err(error);
        }
        let actual_size = std::fs::metadata(&partial)
            .map_err(|error| RemoteFirmwareError::Transport(format!("读取提取镜像失败：{error}")))?
            .len();
        if actual_size != expected_size {
            let _ = std::fs::remove_file(&partial);
            return Err(RemoteFirmwareError::Integrity(format!(
                "分区 {partition_name} 解包字节数不一致：期望 {expected_size}，实际 {actual_size}。"
            )));
        }
        std::fs::rename(&partial, &output_path).map_err(|error| {
            RemoteFirmwareError::Transport(format!("完成提取镜像失败：{error}"))
        })?;
        results.push(ExtractedZipImage {
            partition_name,
            output_path: output_path.to_string_lossy().into_owned(),
            size_bytes: actual_size as i64,
        });
    }
    Ok(results)
}

fn fetch_range<F>(
    url: &str,
    client: Option<&Client>,
    start: u64,
    end: u64,
    is_canceled: &mut F,
) -> Result<Vec<u8>, RemoteFirmwareError>
where
    F: FnMut() -> bool,
{
    let client = client.cloned().unwrap_or_else(default_client);
    fetch_range_bytes(&client, url, start, end, None, is_canceled)
}

/// 拉取 `[start, end]` 的全部字节。
///
/// 逐窗口请求 + 断流续传：单个窗口中途断流（CDN 截断长响应）时，已收到的字节
/// 直接推进偏移，剩余部分用新请求续传，而不是让整个窗口失败——这与 C# 基线
/// `RemoteRangeStream` 的行为一致，且避免「一次断流就丢掉已下载的几 MB」。
fn fetch_range_bytes<F>(
    client: &Client,
    url: &str,
    start: u64,
    end: u64,
    expected_total_len: Option<u64>,
    is_canceled: &mut F,
) -> Result<Vec<u8>, RemoteFirmwareError>
where
    F: FnMut() -> bool,
{
    if end < start {
        return Ok(Vec::new());
    }
    let mut collected = Vec::new();
    let mut offset = start;
    let mut attempts_without_progress = 0u8;
    let mut requests = 0u64;
    while offset <= end {
        if is_canceled() {
            return Err(RemoteFirmwareError::Cancelled);
        }
        requests += 1;
        if requests > RANGE_MAX_REQUESTS {
            return Err(RemoteFirmwareError::Transport(format!(
                "读取远程固件 {start}-{end} 的请求次数超过上限（{RANGE_MAX_REQUESTS} 次）。"
            )));
        }
        let window_end = offset
            .saturating_add(RANGE_SUBREQUEST_BYTES.saturating_sub(1))
            .min(end);
        let (bytes, error) =
            fetch_range_window(client, url, offset, window_end, expected_total_len);
        if !bytes.is_empty() {
            offset += bytes.len() as u64;
            collected.extend_from_slice(&bytes);
            attempts_without_progress = 0;
            // 窗口有字节落袋就继续推进；错误留给下一轮窗口自己暴露。
            continue;
        }
        match error {
            // 协议违规（非 206 / 范围不匹配 / 正文超出声明）：重试无意义，直接失败。
            Some(error) if !is_retryable(&error) => return Err(error),
            Some(error) => {
                attempts_without_progress += 1;
                if attempts_without_progress >= RANGE_MAX_ATTEMPTS_WITHOUT_PROGRESS {
                    return Err(error);
                }
            }
            None => {
                attempts_without_progress += 1;
                if attempts_without_progress >= RANGE_MAX_ATTEMPTS_WITHOUT_PROGRESS {
                    return Err(RemoteFirmwareError::Transport(format!(
                        "读取远程固件 {offset}-{end} 连续 {RANGE_MAX_ATTEMPTS_WITHOUT_PROGRESS} 次未收到数据。"
                    )));
                }
            }
        }
        std::thread::sleep(retry_backoff(attempts_without_progress));
    }
    Ok(collected)
}

fn retry_backoff(attempt: u8) -> Duration {
    let factor = 1u32 << u32::from(attempt.saturating_sub(1).min(4));
    RANGE_RETRY_BACKOFF.saturating_mul(factor)
}

/// 瞬时的传输/截断故障值得重试；取消与协议违规不重试。
fn is_retryable(error: &RemoteFirmwareError) -> bool {
    matches!(error, RemoteFirmwareError::Transport(_))
}

/// 请求单个窗口，返回「已收到的字节」与「是否以错误结束」。
///
/// 两者可能同时非空：服务器发了一半才断开时，那一半仍然有效（调用方据此续传）。
fn fetch_range_window(
    client: &Client,
    url: &str,
    start: u64,
    end: u64,
    expected_total_len: Option<u64>,
) -> (Vec<u8>, Option<RemoteFirmwareError>) {
    let response = match client
        .get(url)
        .header(RANGE, format!("bytes={start}-{end}"))
        .send()
    {
        Ok(response) => response,
        Err(error) => {
            return (
                Vec::new(),
                Some(RemoteFirmwareError::Transport(format!(
                    "请求固件 Range 分段失败：{}",
                    describe_reqwest_error(&error)
                ))),
            )
        }
    };
    let Some(range) = validate_range_response(&response, start, end, expected_total_len) else {
        return (Vec::new(), Some(RemoteFirmwareError::RangeUnsupported));
    };
    read_range_response_body(response, range.body_len)
}

#[derive(Debug, Clone, Copy)]
struct ValidatedRangeResponse {
    total_len: u64,
    body_len: u64,
}

fn validate_range_response(
    response: &reqwest::blocking::Response,
    requested_start: u64,
    requested_end: u64,
    expected_total_len: Option<u64>,
) -> Option<ValidatedRangeResponse> {
    if response.status() != StatusCode::PARTIAL_CONTENT {
        return None;
    }
    let content_range = response
        .headers()
        .get(CONTENT_RANGE)
        .and_then(|value| value.to_str().ok())?;
    let (start, end, total_len) = parse_content_range(content_range)?;
    // 允许服务端只返回请求范围的前一段（RFC 9110 允许）：起点必须一致、终点不得
    // 越过请求上界、总长度必须与探测结果一致。返回更短范围由调用方续传补齐。
    if start != requested_start
        || end > requested_end
        || expected_total_len.is_some_and(|expected| expected != total_len)
    {
        return None;
    }
    let body_len = end.checked_sub(start)?.checked_add(1)?;
    if response
        .content_length()
        .is_some_and(|content_length| content_length != body_len)
    {
        return None;
    }
    Some(ValidatedRangeResponse {
        total_len,
        body_len,
    })
}

fn parse_content_range(value: &str) -> Option<(u64, u64, u64)> {
    let value = value.trim().strip_prefix("bytes ")?;
    let (range, total_len) = value.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    let start = start.trim().parse::<u64>().ok()?;
    let end = end.trim().parse::<u64>().ok()?;
    let total_len = total_len.trim().parse::<u64>().ok()?;
    if start > end || total_len == 0 || end >= total_len {
        return None;
    }
    Some((start, end, total_len))
}

/// 读取响应正文，返回「已收到的字节」与「是否以错误结束」。
///
/// 多读一字节用于识别「正文比声明更长」的协议违规；此时丢弃全部字节并报
/// [`RemoteFirmwareError::RangeUnsupported`]（不可重试）。
fn read_range_response_body(
    response: reqwest::blocking::Response,
    declared_len: u64,
) -> (Vec<u8>, Option<RemoteFirmwareError>) {
    let mut body = response.take(declared_len.saturating_add(1));
    let mut bytes = Vec::new();
    let error = match body.read_to_end(&mut bytes) {
        Ok(_) => None,
        Err(error) => Some(RemoteFirmwareError::Transport(format!(
            "读取固件 Range 响应失败：{}",
            describe_io_error(&error)
        ))),
    };
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > declared_len {
        return (Vec::new(), Some(RemoteFirmwareError::RangeUnsupported));
    }
    (bytes, error)
}

/// 基于 HTTP Range 的只读 + 定位 reader，供 `zip` crate 读取远程 zip。
pub struct RangeHttpReader<'a, F>
where
    F: FnMut() -> bool,
{
    client: Client,
    url: String,
    total_len: u64,
    pos: u64,
    fill: Vec<u8>,
    fill_pos: usize,
    is_canceled: &'a mut F,
}

impl<'a, F> RangeHttpReader<'a, F>
where
    F: FnMut() -> bool,
{
    pub fn new(
        url: &str,
        client: Option<&Client>,
        is_canceled: &'a mut F,
    ) -> Result<Self, RemoteFirmwareError> {
        validate_url(url)?;
        if is_canceled() {
            return Err(RemoteFirmwareError::Cancelled);
        }
        let client = client.cloned().unwrap_or_else(default_client);
        let response = client
            .get(url)
            .header(RANGE, "bytes=0-0")
            .send()
            .map_err(|error| RemoteFirmwareError::Transport(error.to_string()))?;
        let total_len = validate_range_response(&response, 0, 0, None)
            .ok_or(RemoteFirmwareError::RangeUnsupported)?
            .total_len;
        Ok(Self {
            client,
            url: url.to_string(),
            total_len,
            pos: 0,
            fill: Vec::new(),
            fill_pos: 0,
            is_canceled,
        })
    }

    pub fn total_len(&self) -> u64 {
        self.total_len
    }

    fn fetch_from(&mut self, offset: u64, amount: u64) -> Result<(), io::Error> {
        if (self.is_canceled)() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "提取已取消"));
        }
        let end = (offset + amount).saturating_sub(1).min(self.total_len - 1);
        if end < offset {
            self.fill.clear();
            self.fill_pos = 0;
            return Ok(());
        }
        // 断流续传在 fetch_range_bytes 内部完成；这里只把「本次窗口拿到的字节」
        // 装进填充缓冲，短读由 Read 实现继续按新位置补拉。
        let bytes = fetch_range_bytes(
            &self.client,
            &self.url,
            offset,
            end,
            Some(self.total_len),
            &mut *self.is_canceled,
        )
        .map_err(|error| {
            io::Error::other(match error {
                RemoteFirmwareError::Cancelled => "提取已取消".to_string(),
                other => format!("{other}"),
            })
        })?;
        self.fill = bytes;
        self.fill_pos = 0;
        Ok(())
    }
}

impl<'a, F> Read for RangeHttpReader<'a, F>
where
    F: FnMut() -> bool,
{
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        if self.pos >= self.total_len {
            return Ok(0);
        }
        // 复制当前填充缓冲中可用字节。
        let mut copied = 0usize;
        while copied < buffer.len() {
            if self.fill_pos >= self.fill.len() {
                let remaining = self.total_len - self.pos;
                let amount = remaining.min(CHUNK_BYTES);
                self.fetch_from(self.pos, amount)?;
            }
            if self.fill.is_empty() {
                break;
            }
            let available = self.fill.len() - self.fill_pos;
            let need = (buffer.len() - copied).min(available);
            buffer[copied..copied + need]
                .copy_from_slice(&self.fill[self.fill_pos..self.fill_pos + need]);
            self.fill_pos += need;
            self.pos += need as u64;
            copied += need;
        }
        Ok(copied)
    }
}

impl<'a, F> Seek for RangeHttpReader<'a, F>
where
    F: FnMut() -> bool,
{
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        let new_pos = match position {
            SeekFrom::Start(offset) => offset as i128,
            SeekFrom::End(offset) => self.total_len as i128 + offset as i128,
            SeekFrom::Current(offset) => self.pos as i128 + offset as i128,
        };
        if new_pos < 0 || new_pos > self.total_len as i128 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "越界 seek 位置",
            ));
        }
        self.pos = new_pos as u64;
        self.fill.clear();
        self.fill_pos = 0;
        Ok(self.pos)
    }
}

/// 服务器对远程固件包的完整性承诺（`/api/rom` 的 `sha256`/`sizeBytes`）。
/// 两者都可选：上游 VOTA 常为空，服务器只透传。`None` 表示无承诺，
/// 调用方跳过对应校验；一旦给了就是硬门——HTTP 源无 TLS 时，这是唯一
/// 独立于传输层的完整性锚点。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemoteFirmwareIntegrity {
    /// 十六进制（大小写不限）SHA-256 摘要；无效编码视为 `None` 并单独
    /// 报错——服务器"给了但给错"不能静默降级成"没给"。
    pub sha256_hex: Option<String>,
    pub size_bytes: Option<u64>,
}

impl RemoteFirmwareIntegrity {
    /// 解析十六进制摘要为字节。长度非 64 或含非十六进制字符返回 `Err`。
    fn sha256_bytes(&self) -> Result<Option<[u8; 32]>, RemoteFirmwareError> {
        let Some(hex) = self.sha256_hex.as_deref() else {
            return Ok(None);
        };
        let hex = hex.trim();
        if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(RemoteFirmwareError::Integrity(
                "服务器返回的固件 SHA-256 格式无效。".to_string(),
            ));
        }
        let mut digest = [0u8; 32];
        for (index, chunk) in hex.as_bytes().chunks(2).enumerate() {
            let high = (chunk[0] as char)
                .to_digit(16)
                .expect("validated hex digit");
            let low = (chunk[1] as char)
                .to_digit(16)
                .expect("validated hex digit");
            digest[index] = (high * 16 + low) as u8;
        }
        Ok(Some(digest))
    }
}

/// 流式读取整个远程固件包并验证服务器承诺的完整性。
///
/// 按需 Range 提取只下载目标分区字节，所以全包摘要必须独立完整读取
/// 一次——这只有在服务器给出了 sha256/sizeBytes 承诺时才值得花流量。
/// 无任何承诺时直接放行（上游常为空，工具可用性优先，与 C# 行为一致）。
/// 读取按 `CHUNK_BYTES` 分块并逐块触发取消检查。
pub fn verify_remote_firmware_integrity<F>(
    url: &str,
    integrity: &RemoteFirmwareIntegrity,
    client: Option<&Client>,
    is_canceled: &mut F,
) -> Result<(), RemoteFirmwareError>
where
    F: FnMut() -> bool,
{
    let expected_digest = integrity.sha256_bytes()?;
    let expected_size = integrity.size_bytes;
    if expected_digest.is_none() && expected_size.is_none() {
        return Ok(());
    }

    validate_url(url)?;
    let mut reader = RangeHttpReader::new(url, client, is_canceled)?;
    let total_len = reader.total_len();
    if let Some(expected) = expected_size {
        if total_len != expected {
            return Err(RemoteFirmwareError::Integrity(format!(
                "固件包大小校验失败：服务器承诺 {expected} 字节，实际 {total_len} 字节。"
            )));
        }
    }
    if expected_digest.is_none() {
        // 大小已验，无摘要承诺可给。
        return Ok(());
    }

    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; CHUNK_BYTES as usize];
    loop {
        let count = reader
            .read(&mut buffer)
            .map_err(|error| RemoteFirmwareError::Transport(error.to_string()))?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    let actual_digest: [u8; 32] = hasher.finalize().into();
    let expected = expected_digest.expect("digest presence checked above");
    if !constant_time_eq(&actual_digest, &expected) {
        return Err(RemoteFirmwareError::Integrity(
            "固件包 SHA-256 校验失败，下载内容与服务器记录不符，已拒绝提取。".to_string(),
        ));
    }
    Ok(())
}

/// 定长摘要比较：逐字节累积异或，避免短路比较引入的时序侧信道。
fn constant_time_eq(left: &[u8; 32], right: &[u8; 32]) -> bool {
    let mut difference = 0u8;
    for (a, b) in left.iter().zip(right.iter()) {
        difference |= a ^ b;
    }
    difference == 0
}
