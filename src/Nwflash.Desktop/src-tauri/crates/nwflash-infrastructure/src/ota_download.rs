use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use reqwest::{
    header::{CONTENT_LENGTH, CONTENT_RANGE, RANGE},
    Client, Response, StatusCode,
};
use thiserror::Error;
use tokio::{
    fs,
    io::{AsyncSeekExt, AsyncWriteExt},
    sync::Mutex,
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;

pub const OTA_DOWNLOAD_MEMORY_CAP_BYTES: u64 = 256 * 1024 * 1024;
pub const OTA_DOWNLOAD_PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
const OTA_RANGE_PARALLEL_MIN_BYTES: u64 = 1024 * 1024;

/// 单个 Range 子请求的目标字节数。每个并行分片会被再切成若干子块逐个请求：
/// Vivo 固件 CDN（火山引擎 TOS 源站 + 网宿/百度多级 CDN）对长时间的大 Range
/// 响应会中途断流（reqwest 报 `error decoding response body`），一次性请求
/// 整个分片时断流就等于整分片重来。切成小块后断流只损失当前子块，重试从已
/// 落盘偏移续传。
pub const OTA_RANGE_SUBREQUEST_BYTES: u64 = 4 * 1024 * 1024;
/// 单个子块允许的「零进度」重试次数；一旦有字节落盘就重置计数，所以这个上限
/// 只约束「服务器连响应体都不给」的情形，不会限制正常续传。
const OTA_RANGE_SUBREQUEST_MAX_ATTEMPTS: u8 = 5;
/// 单个分片允许的子请求总数上限。正常运行远达不到（4 GiB 分片约 1000 次），
/// 仅作为「服务器每次只回极少字节」时的失控保护。
const OTA_RANGE_SEGMENT_MAX_SUBREQUESTS: u64 = 4096;
/// 重试退避基数，按次数指数放大。
const OTA_RANGE_RETRY_BACKOFF: Duration = Duration::from_millis(400);
/// 建连超时。下载本身不设总超时（多 GB 包会超过任何合理总时限），
/// 用 [`OTA_DOWNLOAD_STALL_TIMEOUT`] 做停滞检测。
pub const OTA_DOWNLOAD_CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// 单次网络读取的停滞超时：超过该时长收不到任何字节即中断本次请求并重试。
pub const OTA_DOWNLOAD_STALL_TIMEOUT: Duration = Duration::from_secs(60);

pub type OtaDownloadProgressSink = dyn Fn(OtaDownloadProgress) + Send + Sync;

#[derive(Debug, Clone, PartialEq)]
pub struct OtaDownloadProgress {
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
    pub bytes_per_second: f64,
}

pub trait OtaDiskSpaceProvider: Send + Sync {
    fn available_bytes(&self, destination: &Path) -> Result<u64, String>;
}

#[derive(Debug, Default)]
pub struct SystemOtaDiskSpaceProvider;

impl OtaDiskSpaceProvider for SystemOtaDiskSpaceProvider {
    fn available_bytes(&self, destination: &Path) -> Result<u64, String> {
        available_disk_bytes(destination)
    }
}

#[derive(Clone)]
pub struct OtaDownloader {
    http_client: Client,
    disk_space: Arc<dyn OtaDiskSpaceProvider>,
    staging_nonce: u64,
}

impl OtaDownloader {
    pub fn new(
        http_client: Client,
        disk_space: Arc<dyn OtaDiskSpaceProvider>,
        staging_nonce: u64,
    ) -> Self {
        Self {
            http_client,
            disk_space,
            staging_nonce,
        }
    }

    pub async fn download_to_file(
        &self,
        url: &str,
        destination: &Path,
        requested_connections: u8,
        cancellation_token: &CancellationToken,
        progress: Option<Arc<OtaDownloadProgressSink>>,
    ) -> Result<u64, OtaDownloadError> {
        self.download_to_file_inner(
            url,
            destination,
            requested_connections,
            cancellation_token,
            progress,
        )
        .await
    }

    async fn download_to_file_inner(
        &self,
        url: &str,
        destination: &Path,
        requested_connections: u8,
        cancellation_token: &CancellationToken,
        progress: Option<Arc<OtaDownloadProgressSink>>,
    ) -> Result<u64, OtaDownloadError> {
        validate_url(url)?;
        let probe = self.probe(url, cancellation_token).await?;
        let plan = plan_ota_download(
            Some(probe.content_length),
            probe.supports_range,
            requested_connections,
        )
        .map_err(map_planning_error)?;
        let available_bytes = self
            .disk_space
            .available_bytes(destination)
            .map_err(OtaDownloadError::Io)?;
        validate_available_space(probe.content_length, available_bytes)
            .map_err(map_planning_error)?;

        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|error| OtaDownloadError::Io(error.to_string()))?;
        }

        let staging = staging_download_path(destination, self.staging_nonce)?;
        let _ = fs::remove_file(&staging).await;
        let outcome = self
            .download_to_staging(url, &staging, &plan, cancellation_token, progress.clone())
            .await;
        // 并发 Range 整体失败（CDN 对多连接同时取同一对象会提前断流）时退化为
        // 单连接续传重下：请求数降为 1，子块续传仍生效，避免用户重头再来。
        let outcome = match outcome {
            Err(error) if error.is_network_failure() => {
                match single_connection_range_fallback(&plan) {
                    Some(fallback) => {
                        let _ = fs::remove_file(&staging).await;
                        self.download_to_staging(
                            url,
                            &staging,
                            &fallback,
                            cancellation_token,
                            progress,
                        )
                        .await
                    }
                    None => Err(error),
                }
            }
            other => other,
        };
        let downloaded = match outcome {
            Ok(downloaded) if downloaded == probe.content_length => downloaded,
            Ok(downloaded) => {
                let _ = fs::remove_file(&staging).await;
                return Err(OtaDownloadError::Download(format!(
                    "下载长度不完整：期望 {} 字节，实际 {downloaded} 字节。",
                    probe.content_length
                )));
            }
            Err(error) => {
                let _ = fs::remove_file(&staging).await;
                return Err(error);
            }
        };

        if cancellation_token.is_cancelled() {
            let _ = fs::remove_file(&staging).await;
            return Err(OtaDownloadError::Cancelled);
        }
        if let Err(error) = commit_staging(&staging, destination).await {
            let _ = fs::remove_file(&staging).await;
            return Err(error);
        }
        Ok(downloaded)
    }

    async fn probe(
        &self,
        url: &str,
        cancellation_token: &CancellationToken,
    ) -> Result<OtaRemoteProbe, OtaDownloadError> {
        let response = tokio::select! {
            _ = cancellation_token.cancelled() => return Err(OtaDownloadError::Cancelled),
            response = self.http_client.head(url).send() => response
                .map_err(|error| OtaDownloadError::Download(format!("探测固件资源失败：{}", describe_http_error(&error))))?,
        };
        if response.status().is_success() {
            return probe_from_head_response(&response);
        }
        if matches!(
            response.status(),
            StatusCode::METHOD_NOT_ALLOWED | StatusCode::NOT_IMPLEMENTED
        ) {
            return self.probe_with_range(url, cancellation_token).await;
        }
        ensure_success(&response).await?;
        unreachable!("successful HEAD responses return above")
    }

    async fn probe_with_range(
        &self,
        url: &str,
        cancellation_token: &CancellationToken,
    ) -> Result<OtaRemoteProbe, OtaDownloadError> {
        let response = tokio::select! {
            _ = cancellation_token.cancelled() => return Err(OtaDownloadError::Cancelled),
            response = self.http_client.get(url).header(RANGE, "bytes=0-0").send() => response
                .map_err(|error| OtaDownloadError::Download(format!("Range 探测固件资源失败：{}", describe_http_error(&error))))?,
        };
        if response.status() != StatusCode::PARTIAL_CONTENT {
            return Err(OtaDownloadError::Download(format!(
                "Range 探测固件资源应返回 HTTP 206，实际为 {}。",
                response.status()
            )));
        }
        let content_length = response
            .headers()
            .get(CONTENT_RANGE)
            .and_then(|value| value.to_str().ok())
            .and_then(content_length_from_range)
            .ok_or(OtaDownloadError::UnknownContentLength)?;
        Ok(OtaRemoteProbe {
            content_length,
            supports_range: true,
        })
    }

    async fn download_to_staging(
        &self,
        url: &str,
        staging: &Path,
        plan: &OtaDownloadPlan,
        cancellation_token: &CancellationToken,
        progress: Option<Arc<OtaDownloadProgressSink>>,
    ) -> Result<u64, OtaDownloadError> {
        // Range 计划一律走分片路径（含单分片）：分片内部按子块续传，
        // 断流只损失当前子块，不会整段重下。
        if let OtaDownloadPlan::RangeParallel {
            range_start,
            range_end,
            connections,
            ..
        } = plan
        {
            let ranges = split_ranges(*range_start, *range_end, *connections);
            return self
                .download_ranges_parallel(
                    url,
                    staging,
                    *range_end + 1,
                    ranges,
                    cancellation_token,
                    progress,
                )
                .await;
        }

        let request = self.http_client.get(url);
        let response = tokio::select! {
            _ = cancellation_token.cancelled() => return Err(OtaDownloadError::Cancelled),
            response = request.send() => response
                .map_err(|error| OtaDownloadError::Download(format!("请求固件资源失败：{}", describe_http_error(&error))))?,
        };
        ensure_response_matches_plan(&response, plan)?;

        let total_bytes = match plan {
            OtaDownloadPlan::RangeParallel { range_end, .. } => range_end.saturating_add(1),
            OtaDownloadPlan::SingleConnection { content_length, .. } => *content_length,
        };
        write_response_to_file(
            response,
            staging,
            total_bytes,
            cancellation_token,
            progress.as_deref(),
        )
        .await
    }

    async fn download_ranges_parallel(
        &self,
        url: &str,
        staging: &Path,
        total_bytes: u64,
        ranges: Vec<(u64, u64)>,
        cancellation_token: &CancellationToken,
        progress: Option<Arc<OtaDownloadProgressSink>>,
    ) -> Result<u64, OtaDownloadError> {
        let file = fs::File::create(staging)
            .await
            .map_err(|error| OtaDownloadError::Io(format!("创建下载文件失败：{error}")))?;
        file.set_len(total_bytes)
            .await
            .map_err(|error| OtaDownloadError::Io(format!("预分配下载文件失败：{error}")))?;
        let file = Arc::new(Mutex::new(file));
        let downloaded = Arc::new(AtomicU64::new(0));
        let progress_state = Arc::new(Mutex::new(ProgressState::new()));
        let worker_cancellation = cancellation_token.child_token();
        let mut workers = JoinSet::new();
        let context = RangeDownloadContext {
            http_client: self.http_client.clone(),
            url: url.to_string(),
            total_bytes,
            output: Arc::clone(&file),
            downloaded: Arc::clone(&downloaded),
            progress_state: Arc::clone(&progress_state),
            cancellation_token: worker_cancellation.clone(),
            progress: progress.clone(),
        };

        for (start, end) in ranges {
            workers.spawn(download_range_segment(context.clone(), start, end));
        }

        let mut first_error = None;
        while let Some(result) = workers.join_next().await {
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    worker_cancellation.cancel();
                    first_error.get_or_insert(error);
                }
                Err(error) => {
                    worker_cancellation.cancel();
                    first_error.get_or_insert_with(|| {
                        OtaDownloadError::Download(format!("Range 下载任务异常：{error}"))
                    });
                }
            }
        }
        if let Some(error) = first_error {
            return Err(error);
        }
        if cancellation_token.is_cancelled() {
            return Err(OtaDownloadError::Cancelled);
        }
        let downloaded = downloaded.load(Ordering::Acquire);
        if downloaded != total_bytes {
            return Err(OtaDownloadError::Download(format!(
                "Range 下载长度不完整：期望 {total_bytes} 字节，实际 {downloaded} 字节。"
            )));
        }
        file.lock()
            .await
            .sync_all()
            .await
            .map_err(|error| OtaDownloadError::Io(format!("落盘失败：{error}")))?;
        report_progress_shared(
            &progress_state,
            progress.as_deref(),
            downloaded,
            total_bytes,
            true,
        )
        .await;
        Ok(downloaded)
    }
}

fn probe_from_head_response(response: &Response) -> Result<OtaRemoteProbe, OtaDownloadError> {
    let content_length = response
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|length| *length > 0)
        .ok_or(OtaDownloadError::UnknownContentLength)?;
    let supports_range = response
        .headers()
        .get("accept-ranges")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("bytes"));
    Ok(OtaRemoteProbe {
        content_length,
        supports_range,
    })
}

fn content_length_from_range(value: &str) -> Option<u64> {
    let (unit_and_range, total) = value.rsplit_once('/')?;
    let (unit, range) = unit_and_range.split_once(char::is_whitespace)?;
    if !unit.eq_ignore_ascii_case("bytes") || range != "0-0" {
        return None;
    }
    total.parse::<u64>().ok().filter(|length| *length > 0)
}

fn split_ranges(range_start: u64, range_end: u64, requested_connections: u8) -> Vec<(u64, u64)> {
    let total_bytes = range_end.saturating_sub(range_start).saturating_add(1);
    if total_bytes <= OTA_RANGE_PARALLEL_MIN_BYTES || requested_connections <= 1 {
        return vec![(range_start, range_end)];
    }

    let connections = u64::from(requested_connections).min(total_bytes);
    let base_size = total_bytes / connections;
    let remainder = total_bytes % connections;
    let mut next_start = range_start;
    let mut ranges = Vec::with_capacity(connections as usize);
    for index in 0..connections {
        let size = base_size + u64::from(index < remainder);
        let end = next_start + size - 1;
        ranges.push((next_start, end));
        next_start = end + 1;
    }
    ranges
}

#[derive(Debug)]
struct ProgressState {
    started: Instant,
    last_report: Option<Instant>,
}

impl ProgressState {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            last_report: None,
        }
    }
}

#[derive(Clone)]
struct RangeDownloadContext {
    http_client: Client,
    url: String,
    total_bytes: u64,
    output: Arc<Mutex<fs::File>>,
    downloaded: Arc<AtomicU64>,
    progress_state: Arc<Mutex<ProgressState>>,
    cancellation_token: CancellationToken,
    progress: Option<Arc<OtaDownloadProgressSink>>,
}

async fn download_range_segment(
    context: RangeDownloadContext,
    range_start: u64,
    range_end: u64,
) -> Result<(), OtaDownloadError> {
    // 逐子块请求 + 断点续传：
    // - 子块成功 → 前进到下一子块（服务端返回更短的合法范围时按实际字节前进）；
    // - 子块中途断流但已有字节落盘 → 从已落盘偏移继续，不重下已写入部分；
    // - 子块零字节进展 → 退避后重试，连续 [`OTA_RANGE_SUBREQUEST_MAX_ATTEMPTS`]
    //   次仍无进展才判本分片失败。
    // 这样 Vivo CDN 的长响应断流只影响当前子块，不会让整个数 GB 下载重来。
    let mut offset = range_start;
    let mut attempts_without_progress = 0u8;
    let mut subrequests = 0u64;
    while offset <= range_end {
        subrequests += 1;
        if subrequests > OTA_RANGE_SEGMENT_MAX_SUBREQUESTS {
            return Err(OtaDownloadError::Download(format!(
                "Range 分段 {range_start}-{range_end} 子请求次数超过上限（{OTA_RANGE_SEGMENT_MAX_SUBREQUESTS} 次），已放弃。"
            )));
        }
        let subrange_end = offset
            .saturating_add(OTA_RANGE_SUBREQUEST_BYTES.saturating_sub(1))
            .min(range_end);
        let mut written = 0u64;
        let result = download_subrange_inner(&context, offset, subrange_end, &mut written).await;
        match result {
            Ok(()) => {
                offset += written.max(1);
                attempts_without_progress = 0;
            }
            Err(error) => {
                if !error.is_retryable_segment_failure() {
                    return Err(error);
                }
                if written > 0 {
                    offset += written;
                    attempts_without_progress = 0;
                    continue;
                }
                attempts_without_progress += 1;
                if attempts_without_progress >= OTA_RANGE_SUBREQUEST_MAX_ATTEMPTS {
                    return Err(error);
                }
                sleep_with_cancellation(
                    &context.cancellation_token,
                    retry_backoff(attempts_without_progress),
                )
                .await?;
            }
        }
    }
    Ok(())
}

/// 指数退避（400ms / 800ms / 1.6s / 3.2s…），并可在退避期间响应取消。
async fn sleep_with_cancellation(
    cancellation_token: &CancellationToken,
    delay: Duration,
) -> Result<(), OtaDownloadError> {
    tokio::select! {
        _ = cancellation_token.cancelled() => Err(OtaDownloadError::Cancelled),
        _ = tokio::time::sleep(delay) => Ok(()),
    }
}

fn retry_backoff(attempt: u8) -> Duration {
    let factor = 1u32 << u32::from(attempt.saturating_sub(1).min(4));
    OTA_RANGE_RETRY_BACKOFF.saturating_mul(factor)
}

async fn download_subrange_inner(
    context: &RangeDownloadContext,
    range_start: u64,
    range_end: u64,
    written: &mut u64,
) -> Result<(), OtaDownloadError> {
    let response = tokio::select! {
        _ = context.cancellation_token.cancelled() => return Err(OtaDownloadError::Cancelled),
        response = context.http_client.get(&context.url).header(RANGE, format!("bytes={range_start}-{range_end}")).send() => response
            .map_err(|error| OtaDownloadError::Download(format!("请求固件 Range 分段失败：{}", describe_http_error(&error))))?,
    };
    // 服务端可以合法地只返回请求范围的一部分，这里按实际返回长度推进。
    let served_bytes =
        ensure_range_response(&response, range_start, range_end, context.total_bytes)?;
    let served_end = range_start.saturating_add(served_bytes).saturating_sub(1);

    let mut response = response;
    let mut offset = range_start;
    loop {
        let chunk = tokio::select! {
            _ = context.cancellation_token.cancelled() => return Err(OtaDownloadError::Cancelled),
            result = tokio::time::timeout(OTA_DOWNLOAD_STALL_TIMEOUT, response.chunk()) => match result {
                Ok(chunk) => chunk,
                Err(_elapsed) => return Err(OtaDownloadError::Download(format!(
                    "Range 分段 {range_start}-{range_end} 连续 {} 秒未收到数据，已中断本次请求。",
                    OTA_DOWNLOAD_STALL_TIMEOUT.as_secs()
                ))),
            },
        };
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(error) => {
                return Err(OtaDownloadError::Download(format!(
                    "Range 下载过程中断：{}",
                    describe_http_error(&error)
                )))
            }
        };
        let Some(chunk) = chunk else {
            break;
        };
        let chunk_length = u64::try_from(chunk.len())
            .map_err(|error| OtaDownloadError::Download(format!("Range 分段大小无效：{error}")))?;
        if offset.saturating_add(chunk_length) > served_end.saturating_add(1) {
            return Err(OtaDownloadError::Download(format!(
                "Range 分段超过声明边界 {range_start}-{served_end}。"
            )));
        }
        {
            let mut output = context.output.lock().await;
            output
                .seek(std::io::SeekFrom::Start(offset))
                .await
                .map_err(|error| OtaDownloadError::Io(format!("定位 Range 输出失败：{error}")))?;
            output
                .write_all(&chunk)
                .await
                .map_err(|error| OtaDownloadError::Io(format!("写入 Range 输出失败：{error}")))?;
        }
        offset += chunk_length;
        *written = written.saturating_add(chunk_length);
        let total_downloaded =
            context.downloaded.fetch_add(chunk_length, Ordering::AcqRel) + chunk_length;
        report_progress_shared(
            &context.progress_state,
            context.progress.as_deref(),
            total_downloaded,
            context.total_bytes,
            false,
        )
        .await;
    }
    if offset != served_end + 1 {
        return Err(OtaDownloadError::Download(format!(
            "Range 分段 {range_start}-{range_end} 不完整。"
        )));
    }
    Ok(())
}

/// 校验 206 Range 响应，返回服务端实际提供的字节数。
///
/// 必须从 `range_start` 起、不越过 `range_end`、总长度与探测结果一致；允许服务端
/// 只返回请求范围的前一段（RFC 9110 允许），调用方按返回长度继续推进。
fn ensure_range_response(
    response: &Response,
    range_start: u64,
    range_end: u64,
    total_bytes: u64,
) -> Result<u64, OtaDownloadError> {
    if response.status() != StatusCode::PARTIAL_CONTENT {
        return Err(OtaDownloadError::Download(format!(
            "Range 固件响应应为 HTTP 206，实际为 {}。",
            response.status()
        )));
    }
    let content_range = response
        .headers()
        .get(CONTENT_RANGE)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| {
            OtaDownloadError::Download("Range 固件响应缺少 Content-Range。".to_string())
        })?;
    let parsed = parse_content_range(content_range).ok_or_else(|| {
        OtaDownloadError::Download(format!("Range 固件响应范围无法解析：{content_range}。"))
    })?;
    if parsed.total != total_bytes {
        return Err(OtaDownloadError::Download(format!(
            "Range 固件响应总长度不匹配：{content_range}，期望 {total_bytes}。"
        )));
    }
    if parsed.start != range_start || parsed.end < parsed.start || parsed.end > range_end {
        return Err(OtaDownloadError::Download(format!(
            "Range 固件响应范围不匹配：{content_range}，请求 {range_start}-{range_end}。"
        )));
    }
    let served_bytes = parsed.end - parsed.start + 1;
    let actual_length = response
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    if actual_length.is_some_and(|length| length != served_bytes) {
        return Err(OtaDownloadError::Download(format!(
            "Range 固件响应长度不匹配：期望 {served_bytes} 字节，声明 {} 字节。",
            actual_length.unwrap_or_default()
        )));
    }
    Ok(served_bytes)
}

struct ParsedContentRange {
    start: u64,
    end: u64,
    total: u64,
}

/// 解析 `bytes {start}-{end}/{total}`。`{total}` 为 `*` 时按无法解析处理。
fn parse_content_range(value: &str) -> Option<ParsedContentRange> {
    let (unit, rest) = value.split_once(char::is_whitespace)?;
    if !unit.eq_ignore_ascii_case("bytes") {
        return None;
    }
    let (range, total) = rest.trim().rsplit_once('/')?;
    let (start, end) = range.split_once('-')?;
    Some(ParsedContentRange {
        start: start.trim().parse().ok()?,
        end: end.trim().parse().ok()?,
        total: total.trim().parse().ok()?,
    })
}

/// 把 reqwest 错误的完整原因链渲染进用户可见文案。reqwest 的 `Display` 只给
/// `error decoding response body` 这类顶层描述，底层 hyper/rustls/IO 原因藏在
/// `source()` 里——不展开就无法区分「连接被重置」「响应体提前结束」
/// 「TLS 未发 close_notify」等完全不同的故障。
fn describe_http_error(error: &reqwest::Error) -> String {
    let mut description = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        description.push_str(" ← ");
        description.push_str(&cause.to_string());
        source = cause.source();
    }
    description
}

async fn report_progress_shared(
    progress_state: &Mutex<ProgressState>,
    progress: Option<&OtaDownloadProgressSink>,
    downloaded_bytes: u64,
    total_bytes: u64,
    force: bool,
) {
    let event = {
        let mut progress_state = progress_state.lock().await;
        let now = Instant::now();
        if !force
            && progress_state.last_report.is_some_and(|previous| {
                now.duration_since(previous) < OTA_DOWNLOAD_PROGRESS_INTERVAL
            })
        {
            None
        } else {
            progress_state.last_report = Some(now);
            let elapsed = now.duration_since(progress_state.started).as_secs_f64();
            Some(OtaDownloadProgress {
                downloaded_bytes,
                total_bytes,
                bytes_per_second: if elapsed > 0.0 {
                    downloaded_bytes as f64 / elapsed
                } else {
                    0.0
                },
            })
        }
    };
    if let (Some(progress), Some(event)) = (progress, event) {
        progress(event);
    }
}

struct OtaRemoteProbe {
    content_length: u64,
    supports_range: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OtaDownloadPlan {
    RangeParallel {
        range_start: u64,
        range_end: u64,
        connections: u8,
        memory_cap_bytes: u64,
    },
    SingleConnection {
        content_length: u64,
        memory_cap_bytes: u64,
    },
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum OtaDownloadPlanningError {
    #[error("无法确定固件包大小。")]
    UnknownContentLength,
    #[error("磁盘空间不足：需要 {required_bytes} 字节，可用 {available_bytes} 字节。")]
    InsufficientDiskSpace {
        required_bytes: u64,
        available_bytes: u64,
    },
}

pub fn plan_ota_download(
    content_length: Option<u64>,
    supports_range: bool,
    requested_connections: u8,
) -> Result<OtaDownloadPlan, OtaDownloadPlanningError> {
    let content_length = content_length
        .filter(|length| *length > 0)
        .ok_or(OtaDownloadPlanningError::UnknownContentLength)?;
    if supports_range {
        Ok(OtaDownloadPlan::RangeParallel {
            range_start: 0,
            range_end: content_length - 1,
            connections: requested_connections.max(1),
            memory_cap_bytes: OTA_DOWNLOAD_MEMORY_CAP_BYTES,
        })
    } else {
        Ok(OtaDownloadPlan::SingleConnection {
            content_length,
            memory_cap_bytes: OTA_DOWNLOAD_MEMORY_CAP_BYTES,
        })
    }
}

pub fn validate_available_space(
    required_bytes: u64,
    available_bytes: u64,
) -> Result<(), OtaDownloadPlanningError> {
    if available_bytes < required_bytes {
        return Err(OtaDownloadPlanningError::InsufficientDiskSpace {
            required_bytes,
            available_bytes,
        });
    }
    Ok(())
}

pub fn staging_download_path(destination: &Path, nonce: u64) -> Result<PathBuf, OtaDownloadError> {
    let name = destination.file_name().ok_or_else(|| {
        OtaDownloadError::InvalidInput("固件下载目标文件名不能为空。".to_string())
    })?;
    Ok(destination.with_file_name(format!(".{}.partial-{nonce}", name.to_string_lossy())))
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum OtaDownloadError {
    #[error("{0}")]
    InvalidInput(String),
    #[error("网络错误: {0}")]
    Download(String),
    #[error("写入失败: {0}")]
    Io(String),
    #[error("无法确定固件包大小。")]
    UnknownContentLength,
    #[error("用户已取消固件下载。")]
    Cancelled,
}

impl OtaDownloadError {
    /// 瞬时分段失败（网络中断/写入抖动）值得重试本分片；取消、输入
    /// 错误与大小未知不重试。
    fn is_retryable_segment_failure(&self) -> bool {
        matches!(self, Self::Download(_) | Self::Io(_))
    }

    /// 纯网络故障：只有这一类才值得退化为单连接重下（磁盘/取消问题重下无意义）。
    fn is_network_failure(&self) -> bool {
        matches!(self, Self::Download(_))
    }
}

/// 多连接 Range 计划整体失败时的降级计划：连接数降为 1，范围不变。
/// 返回 `None` 表示当前计划已经是最小并发，无需降级。
fn single_connection_range_fallback(plan: &OtaDownloadPlan) -> Option<OtaDownloadPlan> {
    match plan {
        OtaDownloadPlan::RangeParallel {
            range_start,
            range_end,
            connections,
            memory_cap_bytes,
        } if *connections > 1 => Some(OtaDownloadPlan::RangeParallel {
            range_start: *range_start,
            range_end: *range_end,
            connections: 1,
            memory_cap_bytes: *memory_cap_bytes,
        }),
        _ => None,
    }
}

/// OTA 下载专用 HTTP 客户端：带建连超时，不设总超时（多 GB 包会超过任何合理
/// 总时限），停滞由每次读取的 [`OTA_DOWNLOAD_STALL_TIMEOUT`] 兜底。
pub fn build_ota_http_client() -> Client {
    Client::builder()
        .connect_timeout(OTA_DOWNLOAD_CONNECT_TIMEOUT)
        .build()
        .unwrap_or_else(|_| Client::new())
}

pub async fn download_to_file(url: &str, destination: &Path) -> Result<u64, OtaDownloadError> {
    download_to_file_with_cancellation(url, destination, &CancellationToken::new(), None).await
}

pub async fn download_to_file_with_cancellation(
    url: &str,
    destination: &Path,
    cancellation_token: &CancellationToken,
    progress: Option<Arc<OtaDownloadProgressSink>>,
) -> Result<u64, OtaDownloadError> {
    OtaDownloader::new(
        build_ota_http_client(),
        Arc::new(SystemOtaDiskSpaceProvider),
        monotonic_nonce(),
    )
    .download_to_file(url, destination, 8, cancellation_token, progress)
    .await
}

pub fn build_download_target_path(root: &Path, _name: &str, pd: &str, version: &str) -> PathBuf {
    let safe_pd = sanitize_component(pd);
    let safe_version = sanitize_component(version);
    root.join(format!("{}_{}_ota.zip", safe_pd, safe_version,))
}

fn sanitize_component(value: &str) -> String {
    value
        .chars()
        .filter(|character| {
            !matches!(
                *character,
                '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'
            )
        })
        .collect()
}

async fn ensure_success(response: &Response) -> Result<(), OtaDownloadError> {
    let status = response.status();
    if status == StatusCode::OK || status == StatusCode::PARTIAL_CONTENT {
        return Ok(());
    }

    Err(OtaDownloadError::Download(format!(
        "获取固件资源失败：HTTP {status}"
    )))
}

async fn write_response_to_file(
    mut response: Response,
    staging: &Path,
    total_bytes: u64,
    cancellation_token: &CancellationToken,
    progress: Option<&OtaDownloadProgressSink>,
) -> Result<u64, OtaDownloadError> {
    let mut file = fs::File::create(staging)
        .await
        .map_err(|error| OtaDownloadError::Io(format!("创建下载文件失败：{error}")))?;

    let mut downloaded = 0u64;
    let started = Instant::now();
    let mut last_report = None;
    loop {
        let chunk = tokio::select! {
            _ = cancellation_token.cancelled() => return Err(OtaDownloadError::Cancelled),
            result = tokio::time::timeout(OTA_DOWNLOAD_STALL_TIMEOUT, response.chunk()) => match result {
                Ok(chunk) => chunk,
                Err(_elapsed) => return Err(OtaDownloadError::Download(format!(
                    "下载连续 {} 秒未收到数据，已中断。",
                    OTA_DOWNLOAD_STALL_TIMEOUT.as_secs()
                ))),
            },
        };
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(error) => {
                return Err(OtaDownloadError::Download(format!(
                    "下载过程中断：{}",
                    describe_http_error(&error)
                )))
            }
        };
        let Some(chunk) = chunk else {
            break;
        };
        let next_downloaded = downloaded
            .checked_add(chunk.len() as u64)
            .ok_or_else(|| OtaDownloadError::Download("下载数据大小溢出。".to_string()))?;
        if next_downloaded > total_bytes {
            return Err(OtaDownloadError::Download(format!(
                "下载数据超过声明大小：期望 {total_bytes} 字节，实际至少 {next_downloaded} 字节。"
            )));
        }
        file.write_all(&chunk)
            .await
            .map_err(|error| OtaDownloadError::Io(format!("写入下载文件失败：{error}")))?;
        downloaded = next_downloaded;
        report_progress(
            progress,
            downloaded,
            total_bytes,
            &started,
            &mut last_report,
            false,
        );
    }

    file.sync_all()
        .await
        .map_err(|error| OtaDownloadError::Io(format!("落盘失败：{error}")))?;
    report_progress(
        progress,
        downloaded,
        total_bytes,
        &started,
        &mut last_report,
        true,
    );

    Ok(downloaded)
}

fn ensure_response_matches_plan(
    response: &Response,
    plan: &OtaDownloadPlan,
) -> Result<(), OtaDownloadError> {
    match plan {
        OtaDownloadPlan::RangeParallel {
            range_start,
            range_end,
            ..
        } => ensure_range_response(response, *range_start, *range_end, range_end + 1).map(|_| ()),
        OtaDownloadPlan::SingleConnection { .. } if response.status() != StatusCode::OK => {
            Err(OtaDownloadError::Download(format!(
                "单连接固件响应应为 HTTP 200，实际为 {}。",
                response.status()
            )))
        }
        _ => Ok(()),
    }
}

async fn commit_staging(staging: &Path, destination: &Path) -> Result<(), OtaDownloadError> {
    fs::rename(staging, destination)
        .await
        .map_err(|error| OtaDownloadError::Io(format!("提交固件下载结果失败：{error}")))
}

fn report_progress(
    progress: Option<&OtaDownloadProgressSink>,
    downloaded_bytes: u64,
    total_bytes: u64,
    started: &Instant,
    last_report: &mut Option<Instant>,
    force: bool,
) {
    let Some(progress) = progress else {
        return;
    };
    let now = Instant::now();
    if !force
        && last_report
            .is_some_and(|previous| now.duration_since(previous) < OTA_DOWNLOAD_PROGRESS_INTERVAL)
    {
        return;
    }
    *last_report = Some(now);
    let elapsed = now.duration_since(*started).as_secs_f64();
    progress(OtaDownloadProgress {
        downloaded_bytes,
        total_bytes,
        bytes_per_second: if elapsed > 0.0 {
            downloaded_bytes as f64 / elapsed
        } else {
            0.0
        },
    });
}

fn map_planning_error(error: OtaDownloadPlanningError) -> OtaDownloadError {
    match error {
        OtaDownloadPlanningError::UnknownContentLength => OtaDownloadError::UnknownContentLength,
        OtaDownloadPlanningError::InsufficientDiskSpace {
            required_bytes,
            available_bytes,
        } => OtaDownloadError::Io(format!(
            "磁盘空间不足：需要 {required_bytes} 字节，可用 {available_bytes} 字节。"
        )),
    }
}

fn monotonic_nonce() -> u64 {
    static NEXT_NONCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    NEXT_NONCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

#[cfg(windows)]
fn available_disk_bytes(destination: &Path) -> Result<u64, String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let path = destination.parent().unwrap_or(destination);
    let mut path_utf16 = path.as_os_str().encode_wide().collect::<Vec<_>>();
    path_utf16.push(0);
    let mut available = 0u64;
    let result = unsafe {
        GetDiskFreeSpaceExW(
            path_utf16.as_ptr(),
            &mut available,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if result == 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(available)
}

#[cfg(not(windows))]
fn available_disk_bytes(_destination: &Path) -> Result<u64, String> {
    Err("当前固件下载实现仅支持 Windows。".to_string())
}

fn validate_url(url: &str) -> Result<(), OtaDownloadError> {
    if url.trim().is_empty() {
        return Err(OtaDownloadError::InvalidInput(
            "固件下载地址不能为空。".to_string(),
        ));
    }
    Ok(())
}
