use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use reqwest::Client;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::{fs as async_fs, io::AsyncWriteExt, time};
use tokio_util::sync::CancellationToken;

use nwflash_domain::DownloadProgress;

use crate::remote_assets::{RemoteAssetSpec, MIRRORS};

#[derive(Debug, Clone)]
pub struct RemoteAssetDownloader {
    http_client: Client,
    mirror_list: Vec<String>,
    pub no_progress_timeout: Duration,
    pub per_candidate_timeout: Duration,
}

#[derive(Debug, Error)]
pub enum ResourceDownloadError {
    #[error("输入参数无效: {0}")]
    InvalidInput(String),

    #[error("用户取消下载。")]
    Cancelled,

    #[error("下载候选源超时: {0}")]
    CandidateTimeout(String),

    #[error("下载 {asset} 失败：连续 {timeout:?} 未有进度，已中断候选源。")]
    NoProgressTimeout { asset: String, timeout: Duration },

    #[error("下载 {asset} 失败。请手动下载后放入提示的位置,或点击以下链接手动获取:\n{manual_url}。{detail}")]
    AllCandidatesFailed {
        asset: String,
        manual_url: String,
        detail: String,
    },

    #[error("下载失败: {0}")]
    Http(String),

    #[error("IO: {0}")]
    Io(String),

    #[error("完整性校验失败: {0}")]
    Integrity(String),
}

pub type ProgressSink = dyn Fn(DownloadProgress) + Send + Sync;

impl Default for RemoteAssetDownloader {
    fn default() -> Self {
        Self::new(None, None, None, None)
    }
}

impl RemoteAssetDownloader {
    pub const DEFAULT_NO_PROGRESS_TIMEOUT: Duration = Duration::from_secs(20);
    pub const DEFAULT_PER_CANDIDATE_TIMEOUT: Duration = Duration::from_secs(5 * 60);

    pub fn new(
        http_client: Option<Client>,
        mirror_list: Option<Vec<String>>,
        no_progress_timeout: Option<Duration>,
        per_candidate_timeout: Option<Duration>,
    ) -> Self {
        let http_client = http_client.unwrap_or_default();

        let mirror_list = mirror_list.unwrap_or_else(|| {
            MIRRORS
                .iter()
                .map(std::string::ToString::to_string)
                .collect()
        });

        Self {
            http_client,
            mirror_list,
            no_progress_timeout: no_progress_timeout.unwrap_or(Self::DEFAULT_NO_PROGRESS_TIMEOUT),
            per_candidate_timeout: per_candidate_timeout
                .unwrap_or(Self::DEFAULT_PER_CANDIDATE_TIMEOUT),
        }
    }

    pub fn build_candidates(&self, github_url: &str) -> Vec<String> {
        let mut candidates = Vec::with_capacity(self.mirror_list.len() + 1);
        candidates.push(github_url.to_string());
        for mirror in &self.mirror_list {
            candidates.push(format!("{}/{github_url}", mirror.trim_end_matches('/')));
        }

        candidates
    }

    pub async fn download_to_file(
        &self,
        spec: &RemoteAssetSpec,
        destination: &Path,
        progress: Option<&ProgressSink>,
        cancellation_token: &CancellationToken,
    ) -> Result<u64, ResourceDownloadError> {
        self.validate(spec)?;
        self.ensure_parent_dir(destination)?;

        let mut last_error: Option<String> = None;

        for (index, candidate) in self
            .build_candidates(&spec.github_url)
            .into_iter()
            .enumerate()
        {
            if cancellation_token.is_cancelled() {
                return Err(ResourceDownloadError::Cancelled);
            }

            let staging = self.staging_path(destination, index);
            self.try_delete_path(&staging);
            let result = time::timeout(
                self.per_candidate_timeout,
                self.download_candidate(&candidate, &staging, spec, progress, cancellation_token),
            )
            .await;

            match result {
                Ok(Ok(downloaded)) => match self.verify(spec, &staging) {
                    Ok(()) => {
                        self.commit_staging(&staging, destination)?;
                        return Ok(downloaded);
                    }
                    Err(error) => {
                        last_error = Some(error.to_string());
                    }
                },
                Ok(Err(error)) => {
                    if matches!(error, ResourceDownloadError::Cancelled) {
                        self.try_delete_path(&staging);
                        return Err(error);
                    }

                    last_error = Some(format!("{error}"));
                }
                Err(timeout) => {
                    last_error = Some(format!("下载候选源 {candidate} 超时: {timeout}"));
                }
            }

            self.try_delete_path(&staging);
        }

        Err(ResourceDownloadError::AllCandidatesFailed {
            asset: spec.display_name.clone(),
            manual_url: spec.github_url.clone(),
            detail: last_error.unwrap_or_else(|| "未发生任何候选请求".to_string()),
        })
    }

    async fn download_candidate(
        &self,
        candidate: &str,
        staging_path: &Path,
        spec: &RemoteAssetSpec,
        progress: Option<&ProgressSink>,
        cancellation_token: &CancellationToken,
    ) -> Result<u64, ResourceDownloadError> {
        let response = tokio::select! {
            _ = cancellation_token.cancelled() => {
                return Err(ResourceDownloadError::Cancelled);
            }
            response = self.http_client.get(candidate).send() => {
                response
                    .map_err(|error| ResourceDownloadError::Http(error.to_string()))?
            }
        };

        if !response.status().is_success() {
            return Err(ResourceDownloadError::Http(format!(
                "{}",
                response.status()
            )));
        }

        let total_bytes = response.content_length();
        let mut response = response;
        let mut output = async_fs::File::create(staging_path)
            .await
            .map_err(|error| ResourceDownloadError::Io(error.to_string()))?;

        let mut downloaded_bytes = 0u64;
        let mut last_speed_checkpoint = Instant::now();
        let mut last_reported_bytes = 0u64;

        loop {
            let chunk = tokio::select! {
                _ = cancellation_token.cancelled() => return Err(ResourceDownloadError::Cancelled),
                chunk = time::timeout(self.no_progress_timeout, response.chunk()) => {
                    chunk
                        .map_err(|_| ResourceDownloadError::NoProgressTimeout {
                            asset: spec.display_name.clone(),
                            timeout: self.no_progress_timeout,
                        })?
                        .map_err(|error| ResourceDownloadError::Io(error.to_string()))?
                }
            };

            let Some(bytes) = chunk else {
                output
                    .flush()
                    .await
                    .map_err(|error| ResourceDownloadError::Io(error.to_string()))?;
                return Ok(downloaded_bytes);
            };

            output
                .write_all(&bytes)
                .await
                .map_err(|error| ResourceDownloadError::Io(error.to_string()))?;

            downloaded_bytes += bytes.len() as u64;
            let now = Instant::now();
            let elapsed = now.duration_since(last_speed_checkpoint);
            let bytes_per_second = if elapsed >= Duration::from_millis(250) {
                let delta_bytes = downloaded_bytes.saturating_sub(last_reported_bytes);
                last_reported_bytes = downloaded_bytes;
                last_speed_checkpoint = now;

                if delta_bytes == 0 {
                    0.0
                } else {
                    (delta_bytes as f64) * 1000.0 / elapsed.as_secs_f64()
                }
            } else {
                0.0
            };

            if let Some(report) = progress {
                report(DownloadProgress {
                    downloaded_bytes: i64::try_from(downloaded_bytes).unwrap_or(i64::MAX),
                    total_bytes: total_bytes.and_then(|bytes| i64::try_from(bytes).ok()),
                    bytes_per_second,
                });
            }
        }
    }

    fn verify(&self, spec: &RemoteAssetSpec, path: &Path) -> Result<(), ResourceDownloadError> {
        let metadata = fs::metadata(path)
            .map_err(|error| ResourceDownloadError::Integrity(error.to_string()))?;
        if metadata.len() == 0 {
            return Err(ResourceDownloadError::Integrity(format!(
                "{} 文件为空。",
                spec.display_name
            )));
        }

        if let Some(expected) = spec.expected_length {
            if metadata.len() != expected {
                return Err(ResourceDownloadError::Integrity(format!(
                    "{} 长度不符(期望 {} 字节,实际 {} 字节)。",
                    spec.display_name,
                    expected,
                    metadata.len()
                )));
            }
        }

        if let Some(expected_sha256) = spec.expected_sha256.as_deref() {
            let actual = compute_sha256(path)?;
            if !hex_equal_ignore_case(actual.as_str(), expected_sha256) {
                return Err(ResourceDownloadError::Integrity(format!(
                    "{} 完整性校验失败(SHA-256 不匹配)。",
                    spec.display_name
                )));
            }
        }

        Ok(())
    }

    fn commit_staging(
        &self,
        staging: &Path,
        destination: &Path,
    ) -> Result<(), ResourceDownloadError> {
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| ResourceDownloadError::Io(error.to_string()))?;
        }

        commit_staging_transactionally_with(staging, destination, |from, to| fs::rename(from, to))
    }

    fn validate(&self, spec: &RemoteAssetSpec) -> Result<(), ResourceDownloadError> {
        if spec.github_url.trim().is_empty() {
            return Err(ResourceDownloadError::InvalidInput(
                "GitHub 下载链接为空。".to_string(),
            ));
        }

        Ok(())
    }

    fn ensure_parent_dir(&self, destination: &Path) -> Result<(), ResourceDownloadError> {
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| ResourceDownloadError::Io(error.to_string()))?;
        }

        Ok(())
    }

    fn staging_path(&self, destination: &Path, attempt: usize) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |value| value.as_nanos());
        let file_name = destination
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("asset");
        destination.with_file_name(format!(".{file_name}-{}.{}", attempt + 1, suffix))
    }

    fn try_delete_path(&self, path: &Path) {
        let _ = fs::remove_file(path);
        let _ = fs::remove_dir_all(path);
    }
}

fn compute_sha256(path: &Path) -> Result<String, ResourceDownloadError> {
    let mut stream =
        File::open(path).map_err(|error| ResourceDownloadError::Io(error.to_string()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 8192];
    loop {
        let read = stream
            .read(&mut buffer)
            .map_err(|error| ResourceDownloadError::Io(error.to_string()))?;
        if read == 0 {
            break;
        }

        hasher.update(&buffer[..read]);
    }

    Ok(format!("{:x}", hasher.finalize()))
}

fn hex_equal_ignore_case(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

fn commit_staging_transactionally_with<F>(
    staging: &Path,
    destination: &Path,
    mut rename: F,
) -> Result<(), ResourceDownloadError>
where
    F: FnMut(&Path, &Path) -> std::io::Result<()>,
{
    if !destination.exists() {
        return rename(staging, destination)
            .map_err(|error| ResourceDownloadError::Io(error.to_string()));
    }

    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |value| value.as_nanos());
    let file_name = destination
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("asset");
    let backup = destination.with_file_name(format!(".{file_name}.backup.{suffix}"));
    rename(destination, &backup).map_err(|error| ResourceDownloadError::Io(error.to_string()))?;

    if let Err(promotion_error) = rename(staging, destination) {
        let restore_error = rename(&backup, destination).err();
        let cleanup_error = fs::remove_file(staging).err();
        return match (restore_error, cleanup_error) {
            (None, None) => Err(ResourceDownloadError::Io(promotion_error.to_string())),
            (restore_error, cleanup_error) => Err(ResourceDownloadError::Io(format!(
                "候选文件发布失败：发布错误: {promotion_error}; 恢复错误: {}; 暂存清理错误: {}; 备份: {}",
                restore_error
                    .map(|error| error.to_string())
                    .unwrap_or_else(|| "无".to_string()),
                cleanup_error
                    .map(|error| error.to_string())
                    .unwrap_or_else(|| "无".to_string()),
                backup.display()
            ))),
        };
    }

    fs::remove_file(&backup).map_err(|error| ResourceDownloadError::Io(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_staging_promotion_restores_the_previously_approved_asset() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be available")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("nwflash-commit-rollback-{suffix}"));
        fs::create_dir_all(&root).expect("fixture directory should be created");
        let destination = root.join("approved.exe");
        let staging = root.join("candidate.exe");
        fs::write(&destination, b"approved").expect("approved fixture should be written");
        fs::write(&staging, b"candidate").expect("candidate fixture should be written");
        let mut rename_attempt = 0;

        let error = commit_staging_transactionally_with(&staging, &destination, |from, to| {
            rename_attempt += 1;
            if rename_attempt == 2 {
                Err(std::io::Error::other("injected promotion failure"))
            } else {
                fs::rename(from, to)
            }
        })
        .expect_err("failed candidate promotion should be reported");

        assert!(error.to_string().contains("injected promotion failure"));
        assert_eq!(
            fs::read(&destination).expect("approved asset should be restored"),
            b"approved"
        );
        assert!(
            !staging.exists(),
            "failed candidate staging should be removed before the error escapes"
        );
        assert_eq!(rename_attempt, 3, "the backup should be restored");
        assert_eq!(
            fs::read_dir(&root)
                .expect("fixture directory should be readable")
                .count(),
            1,
            "only the restored approved asset should remain"
        );
        fs::remove_dir_all(root).expect("fixture directory should be removed");
    }
}
