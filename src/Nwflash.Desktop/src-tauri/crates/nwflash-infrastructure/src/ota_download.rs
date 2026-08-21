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
            .download_to_staging(url, &staging, &plan, cancellation_token, progress)
            .await;
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
                .map_err(|error| OtaDownloadError::Download(format!("探测 OTA 资源失败：{error}")))?,
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
                .map_err(|error| OtaDownloadError::Download(format!("Range 探测 OTA 资源失败：{error}")))?,
        };
        if response.status() != StatusCode::PARTIAL_CONTENT {
            return Err(OtaDownloadError::Download(format!(
                "Range 探测 OTA 资源应返回 HTTP 206，实际为 {}。",
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
        if let OtaDownloadPlan::RangeParallel {
            range_start,
            range_end,
            connections,
            ..
        } = plan
        {
            let ranges = split_ranges(*range_start, *range_end, *connections);
            if ranges.len() > 1 {
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
        }

        let request = match plan {
            OtaDownloadPlan::RangeParallel {
                range_start,
                range_end,
                ..
            } => self
                .http_client
                .get(url)
                .header(RANGE, format!("bytes={range_start}-{range_end}")),
            OtaDownloadPlan::SingleConnection { .. } => self.http_client.get(url),
        };
        let response = tokio::select! {
            _ = cancellation_token.cancelled() => return Err(OtaDownloadError::Cancelled),
            response = request.send() => response
                .map_err(|error| OtaDownloadError::Download(format!("请求 OTA 资源失败：{error}")))?,
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

        for (start, end) in ranges {
            workers.spawn(download_range_segment(
                self.http_client.clone(),
                url.to_string(),
                start,
                end,
                total_bytes,
                Arc::clone(&file),
                Arc::clone(&downloaded),
                Arc::clone(&progress_state),
                worker_cancellation.clone(),
                progress.clone(),
            ));
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

#[allow(clippy::too_many_arguments)]
async fn download_range_segment(
    http_client: Client,
    url: String,
    range_start: u64,
    range_end: u64,
    total_bytes: u64,
    output: Arc<Mutex<fs::File>>,
    downloaded: Arc<AtomicU64>,
    progress_state: Arc<Mutex<ProgressState>>,
    cancellation_token: CancellationToken,
    progress: Option<Arc<OtaDownloadProgressSink>>,
) -> Result<(), OtaDownloadError> {
    let response = tokio::select! {
        _ = cancellation_token.cancelled() => return Err(OtaDownloadError::Cancelled),
        response = http_client.get(&url).header(RANGE, format!("bytes={range_start}-{range_end}")).send() => response
            .map_err(|error| OtaDownloadError::Download(format!("请求 OTA Range 分段失败：{error}")))?,
    };
    ensure_range_response(&response, range_start, range_end, total_bytes)?;

    let mut response = response;
    let mut offset = range_start;
    loop {
        let chunk = tokio::select! {
            _ = cancellation_token.cancelled() => return Err(OtaDownloadError::Cancelled),
            chunk = response.chunk() => chunk
                .map_err(|error| OtaDownloadError::Download(format!("Range 下载过程中断：{error}")))?,
        };
        let Some(chunk) = chunk else {
            break;
        };
        let chunk_length = u64::try_from(chunk.len())
            .map_err(|error| OtaDownloadError::Download(format!("Range 分段大小无效：{error}")))?;
        if offset.saturating_add(chunk_length) > range_end.saturating_add(1) {
            return Err(OtaDownloadError::Download(format!(
                "Range 分段超过声明边界 {range_start}-{range_end}。"
            )));
        }
        {
            let mut output = output.lock().await;
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
        let total_downloaded = downloaded.fetch_add(chunk_length, Ordering::AcqRel) + chunk_length;
        report_progress_shared(
            &progress_state,
            progress.as_deref(),
            total_downloaded,
            total_bytes,
            false,
        )
        .await;
    }
    if offset != range_end + 1 {
        return Err(OtaDownloadError::Download(format!(
            "Range 分段 {range_start}-{range_end} 不完整。"
        )));
    }
    Ok(())
}

fn ensure_range_response(
    response: &Response,
    range_start: u64,
    range_end: u64,
    total_bytes: u64,
) -> Result<(), OtaDownloadError> {
    if response.status() != StatusCode::PARTIAL_CONTENT {
        return Err(OtaDownloadError::Download(format!(
            "Range OTA 响应应为 HTTP 206，实际为 {}。",
            response.status()
        )));
    }
    let content_range = response
        .headers()
        .get(CONTENT_RANGE)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| {
            OtaDownloadError::Download("Range OTA 响应缺少 Content-Range。".to_string())
        })?;
    if content_range != format!("bytes {range_start}-{range_end}/{total_bytes}") {
        return Err(OtaDownloadError::Download(format!(
            "Range OTA 响应范围不匹配：{content_range}。"
        )));
    }
    let expected_length = range_end - range_start + 1;
    let actual_length = response
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    if actual_length != Some(expected_length) {
        return Err(OtaDownloadError::Download(format!(
            "Range OTA 响应长度不匹配：期望 {expected_length} 字节。"
        )));
    }
    Ok(())
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
    #[error("无法确定 OTA 包大小。")]
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
        OtaDownloadError::InvalidInput("OTA 下载目标文件名不能为空。".to_string())
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
    #[error("无法确定 OTA 包大小。")]
    UnknownContentLength,
    #[error("用户已取消 OTA 下载。")]
    Cancelled,
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
        Client::new(),
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
        "获取 OTA 资源失败：HTTP {status}"
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
            chunk = response.chunk() => chunk
                .map_err(|error| OtaDownloadError::Download(format!("下载过程中断：{error}")))?,
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
        } => ensure_range_response(response, *range_start, *range_end, range_end + 1),
        OtaDownloadPlan::SingleConnection { .. } if response.status() != StatusCode::OK => {
            Err(OtaDownloadError::Download(format!(
                "单连接 OTA 响应应为 HTTP 200，实际为 {}。",
                response.status()
            )))
        }
        _ => Ok(()),
    }
}

async fn commit_staging(staging: &Path, destination: &Path) -> Result<(), OtaDownloadError> {
    fs::rename(staging, destination)
        .await
        .map_err(|error| OtaDownloadError::Io(format!("提交 OTA 下载结果失败：{error}")))
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
    Err("当前 OTA 下载实现仅支持 Windows。".to_string())
}

fn validate_url(url: &str) -> Result<(), OtaDownloadError> {
    if url.trim().is_empty() {
        return Err(OtaDownloadError::InvalidInput(
            "OTA 下载地址不能为空。".to_string(),
        ));
    }
    Ok(())
}
