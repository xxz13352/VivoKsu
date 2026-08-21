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

use reqwest::blocking::Client;
use reqwest::header::{CONTENT_RANGE, RANGE};
use reqwest::StatusCode;
use thiserror::Error;
use url::Url;
use zip::ZipArchive;

/// 每次网络拉取的填充块上限（字节）。非 0 保证取消检查按块触发。
const CHUNK_BYTES: u64 = 1024 * 1024;

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
    #[error("OTA 地址不能为空。")]
    InvalidUrl(String),
    #[error("读取远程 OTA 失败：{0}")]
    Transport(String),
    #[error("远程 OTA 服务器不支持 Range 请求。")]
    RangeUnsupported,
    #[error("不支持的 OTA 格式。")]
    UnsupportedFormat,
    #[error("读取 OTA 压缩包失败：{0}")]
    Archive(String),
    #[error("OTA 中不存在分区 {0}。")]
    MissingPartition(String),
    #[error("OTA 分区完整性校验失败：{0}")]
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
        .build()
        .expect("reqwest blocking client should build")
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
    let body = fetch_range(url, client, 0, 3)?;
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

fn fetch_range(
    url: &str,
    client: Option<&Client>,
    start: u64,
    mut end: u64,
) -> Result<Vec<u8>, RemoteFirmwareError> {
    if end < start {
        return Ok(Vec::new());
    }
    // 单次优先小请求（探测只用 4 字节）；这里由调用方保证跨度。
    if end - start > 32 * 1024 * 1024 {
        end = start + 32 * 1024 * 1024 - 1;
    }
    let client = client.cloned().unwrap_or_else(default_client);
    let response = client
        .get(url)
        .header(RANGE, format!("bytes={start}-{end}"))
        .send()
        .map_err(|error| RemoteFirmwareError::Transport(error.to_string()))?;
    let range = validate_range_response(&response, start, end, None)
        .ok_or(RemoteFirmwareError::RangeUnsupported)?;
    read_range_response_body(response, range)
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
    if start != requested_start
        || end > requested_end
        || (end < requested_end && end.checked_add(1) != Some(total_len))
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

fn read_range_response_body(
    response: reqwest::blocking::Response,
    range: ValidatedRangeResponse,
) -> Result<Vec<u8>, RemoteFirmwareError> {
    let limit = range
        .body_len
        .checked_add(1)
        .ok_or(RemoteFirmwareError::RangeUnsupported)?;
    let mut body = response.take(limit);
    let mut bytes = Vec::new();
    body.read_to_end(&mut bytes)
        .map_err(|error| RemoteFirmwareError::Transport(error.to_string()))?;
    if u64::try_from(bytes.len()).ok() != Some(range.body_len) {
        return Err(RemoteFirmwareError::RangeUnsupported);
    }
    Ok(bytes)
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
        let response = self
            .client
            .get(&self.url)
            .header(RANGE, format!("bytes={offset}-{end}"))
            .send()
            .map_err(|error| io::Error::other(error.to_string()))?;
        let range = validate_range_response(&response, offset, end, Some(self.total_len))
            .ok_or_else(|| io::Error::other("远程 OTA 服务器不支持有效的 Range 请求。"))?;
        self.fill = read_range_response_body(response, range)
            .map_err(|_| io::Error::other("远程 OTA 服务器不支持有效的 Range 请求。"))?;
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
