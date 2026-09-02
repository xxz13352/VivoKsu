use std::{
    collections::{BTreeMap, HashSet},
    fmt,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use nwflash_application::{
    FirmwareExtractApplicationError, FirmwareExtractEntry, FirmwareExtractInspection,
    FirmwareExtractService,
};
use nwflash_infrastructure::remote_firmware::{
    extract_zip_members, list_zip_members, probe_remote_kind, validate_http_url,
    RemoteFirmwareError, RemoteFirmwareKind, ZipMember,
};
use serde::Serialize;
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;
use tokio::{sync::oneshot, task};
use uuid::Uuid;

use crate::AppState;

pub(crate) const FIRMWARE_PROGRESS_EVENT: &str = "firmware:progress";
const FIRMWARE_PROGRESS_THROTTLE: Duration = Duration::from_millis(100);
const FIRMWARE_OUTPUT_DIRECTORY_SELECTION_ERROR: &str = "提取输出目录选择已失效，请重新选择。";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FirmwareProgressDto {
    pub(crate) current_partition: Option<String>,
    pub(crate) current_partition_index: Option<usize>,
    pub(crate) total_partitions: usize,
    pub(crate) completed_partitions: usize,
    pub(crate) successful_partitions: usize,
    pub(crate) failed_partitions: usize,
    pub(crate) skipped_partitions: usize,
    pub(crate) bytes_completed: u64,
    pub(crate) bytes_total: u64,
    pub(crate) percentage: f64,
    pub(crate) bytes_per_second: f64,
    pub(crate) elapsed_milliseconds: u128,
    pub(crate) gzip_stream_bytes: Option<u64>,
}

type FirmwareProgressSink = dyn Fn(FirmwareProgressDto) + Send + Sync;

#[derive(Clone)]
pub(crate) struct FirmwareProgressRuntime {
    sink: Arc<Mutex<Arc<FirmwareProgressSink>>>,
}

impl Default for FirmwareProgressRuntime {
    fn default() -> Self {
        Self::with_sink(|_| {})
    }
}

impl FirmwareProgressRuntime {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn with_sink<F>(sink: F) -> Self
    where
        F: Fn(FirmwareProgressDto) + Send + Sync + 'static,
    {
        Self {
            sink: Arc::new(Mutex::new(Arc::new(sink))),
        }
    }

    pub(crate) fn bind_sink<F>(&self, sink: F)
    where
        F: Fn(FirmwareProgressDto) + Send + Sync + 'static,
    {
        if let Ok(mut current) = self.sink.lock() {
            *current = Arc::new(sink);
        }
    }

    fn start(&self) -> FirmwareProgressReporter {
        let sink = self
            .sink
            .lock()
            .map(|sink| sink.clone())
            .unwrap_or_else(|_| Arc::new(|_| {}));
        FirmwareProgressReporter {
            sink,
            started_at: Instant::now(),
            last_emitted_at: Arc::new(Mutex::new(None)),
            partition_stats: Arc::new(Mutex::new(FirmwarePartitionStats::default())),
        }
    }
}

#[derive(Clone)]
struct FirmwareProgressReporter {
    sink: Arc<FirmwareProgressSink>,
    started_at: Instant,
    last_emitted_at: Arc<Mutex<Option<Instant>>>,
    partition_stats: Arc<Mutex<FirmwarePartitionStats>>,
}

#[derive(Default)]
struct FirmwarePartitionStats {
    total: usize,
    completed: usize,
    successful: usize,
    failed: usize,
    skipped: usize,
    current_index: Option<usize>,
    seen: HashSet<String>,
}

impl FirmwareProgressReporter {
    fn report(
        &self,
        current_partition: Option<String>,
        bytes_completed: u64,
        bytes_total: u64,
        gzip_stream_bytes: Option<u64>,
    ) {
        self.update_partition_stats(current_partition.as_deref(), false);
        self.emit(
            current_partition,
            bytes_completed,
            bytes_total,
            gzip_stream_bytes,
            false,
        );
    }

    fn report_terminal(
        &self,
        current_partition: Option<String>,
        bytes_completed: u64,
        bytes_total: u64,
        gzip_stream_bytes: Option<u64>,
    ) {
        self.update_partition_stats(current_partition.as_deref(), true);
        self.emit(
            current_partition,
            bytes_completed,
            bytes_total,
            gzip_stream_bytes,
            true,
        );
    }

    fn set_total_partitions(&self, total: usize) {
        if let Ok(mut stats) = self.partition_stats.lock() {
            stats.total = total;
        }
    }

    fn total_partitions(&self) -> usize {
        self.partition_stats
            .lock()
            .map(|stats| stats.total)
            .unwrap_or_default()
    }

    fn update_partition_stats(&self, current_partition: Option<&str>, terminal: bool) {
        let Ok(mut stats) = self.partition_stats.lock() else {
            return;
        };
        if let Some(partition) = current_partition.filter(|value| !value.is_empty()) {
            if stats.seen.insert(partition.to_string()) {
                stats.current_index = Some(stats.seen.len());
                if stats.seen.len() > 1 {
                    stats.successful = stats.successful.saturating_add(1);
                }
                stats.completed = stats.seen.len().saturating_sub(1);
            }
        }
        if terminal {
            stats.completed = stats.total.max(stats.seen.len());
            stats.successful = stats.completed;
            stats.current_index = None;
        }
    }

    fn emit(
        &self,
        current_partition: Option<String>,
        bytes_completed: u64,
        bytes_total: u64,
        gzip_stream_bytes: Option<u64>,
        force: bool,
    ) {
        let now = Instant::now();
        let mut last_emitted_at = match self.last_emitted_at.lock() {
            Ok(value) => value,
            Err(_) => return,
        };
        if !force
            && last_emitted_at
                .is_some_and(|last| now.duration_since(last) < FIRMWARE_PROGRESS_THROTTLE)
        {
            return;
        }
        *last_emitted_at = Some(now);
        drop(last_emitted_at);
        let elapsed = now.duration_since(self.started_at);
        let elapsed_milliseconds = elapsed.as_millis();
        let percentage = if bytes_total == 0 {
            0.0
        } else {
            (bytes_completed as f64 / bytes_total as f64 * 100.0).min(100.0)
        };
        let bytes_per_second = if elapsed.is_zero() {
            0.0
        } else {
            bytes_completed as f64 / elapsed.as_secs_f64()
        };
        let (
            current_partition_index,
            total_partitions,
            completed_partitions,
            successful_partitions,
            failed_partitions,
            skipped_partitions,
        ) = self
            .partition_stats
            .lock()
            .map(|stats| {
                (
                    stats.current_index,
                    stats.total,
                    stats.completed,
                    stats.successful,
                    stats.failed,
                    stats.skipped,
                )
            })
            .unwrap_or_default();
        (self.sink)(FirmwareProgressDto {
            current_partition,
            current_partition_index,
            total_partitions,
            completed_partitions,
            successful_partitions,
            failed_partitions,
            skipped_partitions,
            bytes_completed,
            bytes_total,
            percentage,
            bytes_per_second,
            elapsed_milliseconds,
            gzip_stream_bytes,
        });
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FirmwareExtractEntryDto {
    pub id: String,
    pub name: String,
    pub size_bytes: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FirmwareInspectionDto {
    pub format: String,
    pub entries: Vec<FirmwareExtractEntryDto>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractedFirmwareImageDto {
    pub name: String,
    pub size_bytes: i64,
    pub result_id: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FirmwareExtractionDto {
    pub images: Vec<ExtractedFirmwareImageDto>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FirmwareOutputDirectorySelectionDto {
    pub selection_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparedFirmwareArtifactDto {
    pub artifact_id: String,
    pub name: String,
    pub size_bytes: i64,
}

static FIRMWARE_ARTIFACT_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static FIRMWARE_EXTRACTION_RESULT_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
pub(crate) struct FirmwareArtifact {
    pub(crate) partition: nwflash_domain::QuickFlashPartition,
    pub(crate) image: nwflash_domain::FlashImageInfo,
    pub(crate) staging_root: PathBuf,
    cleanup_staging_root: bool,
}

#[derive(Debug)]
struct InternalFirmwareStagingRoot(PathBuf);

impl InternalFirmwareStagingRoot {
    fn as_path(&self) -> &Path {
        &self.0
    }

    fn into_path(self) -> PathBuf {
        self.0
    }
}

#[derive(Clone, Default)]
pub struct FirmwareArtifactRuntime {
    artifact: Arc<Mutex<Option<(String, FirmwareArtifact)>>>,
}

impl FirmwareArtifactRuntime {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn replace(
        &self,
        partition: nwflash_domain::QuickFlashPartition,
        image: nwflash_domain::FlashImageInfo,
        staging_root: PathBuf,
    ) -> String {
        self.replace_with_ownership(partition, image, staging_root, false)
    }

    fn replace_owned(
        &self,
        partition: nwflash_domain::QuickFlashPartition,
        image: nwflash_domain::FlashImageInfo,
        staging_root: InternalFirmwareStagingRoot,
    ) -> String {
        self.replace_with_ownership(partition, image, staging_root.into_path(), true)
    }

    fn replace_with_ownership(
        &self,
        partition: nwflash_domain::QuickFlashPartition,
        image: nwflash_domain::FlashImageInfo,
        staging_root: PathBuf,
        cleanup_staging_root: bool,
    ) -> String {
        let artifact_id = format!(
            "firmware-{}",
            FIRMWARE_ARTIFACT_SEQUENCE.fetch_add(1, Ordering::Relaxed) + 1
        );
        let replaced = self
            .artifact
            .lock()
            .expect("firmware artifact lock should not be poisoned")
            .replace((
                artifact_id.clone(),
                FirmwareArtifact {
                    partition,
                    image,
                    staging_root,
                    cleanup_staging_root,
                },
            ));
        if let Some((_, artifact)) = replaced.filter(|(_, artifact)| artifact.cleanup_staging_root)
        {
            let _ = fs::remove_dir_all(artifact.staging_root);
        }
        artifact_id
    }

    pub(crate) fn get(&self, artifact_id: &str) -> Result<FirmwareArtifact, String> {
        self.artifact
            .lock()
            .expect("firmware artifact lock should not be poisoned")
            .as_ref()
            .filter(|(stored_id, _)| stored_id == artifact_id)
            .map(|(_, artifact)| artifact.clone())
            .ok_or_else(|| "固件提取结果已失效，请重新提取。".to_string())
    }

    pub(crate) fn clear_owned(&self) -> Vec<PathBuf> {
        let artifact = self
            .artifact
            .lock()
            .expect("firmware artifact lock should not be poisoned")
            .take();
        artifact
            .filter(|(_, artifact)| artifact.cleanup_staging_root)
            .map(|(_, artifact)| vec![artifact.staging_root])
            .unwrap_or_default()
    }
}

#[cfg(test)]
pub(crate) fn register_owned_firmware_artifact_for_test(
    runtime: &FirmwareArtifactRuntime,
    partition: nwflash_domain::QuickFlashPartition,
    image: nwflash_domain::FlashImageInfo,
    staging_root: PathBuf,
) -> String {
    runtime.replace_owned(partition, image, InternalFirmwareStagingRoot(staging_root))
}

#[derive(Debug, Clone)]
struct FirmwareExtractionResult {
    name: String,
    partition: nwflash_domain::QuickFlashPartition,
    image: nwflash_domain::FlashImageInfo,
}

#[derive(Default)]
struct FirmwareExtractionStore {
    images: BTreeMap<String, FirmwareExtractionResult>,
    staging_root: Option<PathBuf>,
}

impl Drop for FirmwareExtractionStore {
    fn drop(&mut self) {
        if let Some(staging_root) = self.staging_root.take() {
            let _ = fs::remove_dir_all(staging_root);
        }
    }
}

#[derive(Clone, Default)]
pub struct FirmwareExtractionRuntime {
    store: Arc<Mutex<FirmwareExtractionStore>>,
}

impl FirmwareExtractionRuntime {
    pub fn new() -> Self {
        Self::default()
    }

    fn replace(
        &self,
        images: Vec<nwflash_domain::FlashImageInfo>,
    ) -> Result<Vec<Option<String>>, String> {
        let mut replacement = FirmwareExtractionStore::default();
        let mut result_ids = Vec::with_capacity(images.len());

        for image in images {
            let source = Path::new(&image.path);
            let name = source
                .file_name()
                .and_then(|name| name.to_str())
                .filter(|name| !name.is_empty())
                .unwrap_or("镜像")
                .to_string();
            let partition = match quick_flash_partition_from_name(&name) {
                Ok(partition) if flashable_image_extension(source).is_some() => partition,
                _ => {
                    result_ids.push(None);
                    continue;
                }
            };
            if replacement.staging_root.is_none() {
                replacement.staging_root =
                    Some(create_unique_firmware_root("firmware-result")?.into_path());
            }
            let result_id = format!(
                "result-{}",
                FIRMWARE_EXTRACTION_RESULT_SEQUENCE.fetch_add(1, Ordering::Relaxed) + 1
            );
            let extension = flashable_image_extension(source).unwrap_or("img");
            let snapshot_path = replacement
                .staging_root
                .as_ref()
                .expect("result staging root should exist")
                .join(format!("{result_id}.{extension}"));
            copy_image_snapshot(source, &snapshot_path, image.size_bytes)?;
            replacement.images.insert(
                result_id.clone(),
                FirmwareExtractionResult {
                    name,
                    partition,
                    image: nwflash_domain::FlashImageInfo {
                        path: snapshot_path.to_string_lossy().into_owned(),
                        size_bytes: image.size_bytes,
                    },
                },
            );
            result_ids.push(Some(result_id));
        }

        let mut stored = self
            .store
            .lock()
            .expect("firmware extraction results lock should not be poisoned");
        let previous = std::mem::replace(&mut *stored, replacement);
        drop(stored);
        drop(previous);
        Ok(result_ids)
    }

    fn clear(&self) {
        let mut stored = self
            .store
            .lock()
            .expect("firmware extraction results lock should not be poisoned");
        let previous = std::mem::take(&mut *stored);
        drop(stored);
        drop(previous);
    }

    fn get(&self, result_id: &str) -> Result<FirmwareExtractionResult, String> {
        self.store
            .lock()
            .expect("firmware extraction results lock should not be poisoned")
            .images
            .get(result_id)
            .cloned()
            .ok_or_else(|| "固件提取结果已失效，请重新提取。".to_string())
    }
}

#[derive(Clone, Default)]
pub struct PayloadInspectionRuntime {
    inspection: Arc<Mutex<Option<PayloadInspection>>>,
}

#[derive(Debug, Clone)]
struct PayloadInspection {
    source: String,
    entries: Vec<FirmwareExtractEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PayloadExtractionSelection {
    source: String,
    entries: Vec<FirmwareExtractEntry>,
    total_bytes: u64,
}

impl PayloadInspectionRuntime {
    pub fn new() -> Self {
        Self::default()
    }

    fn replace(&self, source: String, entries: Vec<FirmwareExtractEntry>) {
        *self
            .inspection
            .lock()
            .expect("payload inspection lock should not be poisoned") =
            Some(PayloadInspection { source, entries });
    }

    fn clear(&self) {
        self.inspection
            .lock()
            .expect("payload inspection lock should not be poisoned")
            .take();
    }

    pub(crate) fn resolve_selected(
        &self,
        selected_ids: &[String],
    ) -> Result<PayloadExtractionSelection, String> {
        if selected_ids.is_empty() {
            return Err("请选择有效且不重复的 payload 分区。".to_string());
        }
        let inspection = self
            .inspection
            .lock()
            .expect("payload inspection lock should not be poisoned")
            .clone()
            .ok_or_else(|| "payload 分区列表已失效，请重新读取。".to_string())?;
        let mut seen = HashSet::new();
        let mut entries = Vec::with_capacity(selected_ids.len());
        let mut total_bytes = 0u64;
        for id in selected_ids {
            if !seen.insert(id) {
                return Err("请选择有效且不重复的 payload 分区。".to_string());
            }
            let entry = inspection
                .entries
                .iter()
                .find(|entry| entry.id == *id)
                .ok_or_else(|| "请选择有效且不重复的 payload 分区。".to_string())?;
            entries.push(entry.clone());
            total_bytes = total_bytes.saturating_add(u64::try_from(entry.size_bytes).unwrap_or(0));
        }
        Ok(PayloadExtractionSelection {
            source: inspection.source,
            entries,
            total_bytes,
        })
    }
}

#[derive(Clone, Default)]
pub struct RemoteFirmwareInspectionRuntime {
    inspection: Arc<Mutex<Option<RemoteFirmwareInspection>>>,
}

#[derive(Clone, Default)]
pub struct FirmwareOutputDirectoryRuntime {
    state: Arc<Mutex<FirmwareOutputDirectoryState>>,
}

#[derive(Default)]
struct FirmwareOutputDirectoryState {
    picker_epoch: u64,
    selection: Option<(String, PathBuf)>,
}

#[derive(Clone, Copy)]
struct FirmwareOutputDirectoryPickerLease {
    epoch: u64,
}

impl FirmwareOutputDirectoryRuntime {
    pub fn new() -> Self {
        Self::default()
    }

    #[cfg(test)]
    pub(crate) fn replace(&self, directory: PathBuf) -> FirmwareOutputDirectorySelectionDto {
        let picker = self.begin_selection();
        self.publish_selection(picker, Some(directory))
            .expect("a newly opened picker should remain current")
            .expect("a selected directory should publish a capability")
    }

    fn begin_selection(&self) -> FirmwareOutputDirectoryPickerLease {
        let mut state = self
            .state
            .lock()
            .expect("firmware output directory lock should not be poisoned");
        state.picker_epoch = state
            .picker_epoch
            .checked_add(1)
            .expect("firmware output directory picker epoch exhausted");
        FirmwareOutputDirectoryPickerLease {
            epoch: state.picker_epoch,
        }
    }

    fn publish_selection(
        &self,
        picker: FirmwareOutputDirectoryPickerLease,
        selected: Option<PathBuf>,
    ) -> Result<Option<FirmwareOutputDirectorySelectionDto>, String> {
        let mut state = self
            .state
            .lock()
            .expect("firmware output directory lock should not be poisoned");
        if picker.epoch != state.picker_epoch {
            return Err(FIRMWARE_OUTPUT_DIRECTORY_SELECTION_ERROR.to_string());
        }
        let Some(directory) = selected else {
            return Ok(None);
        };
        let selection_id = format!("firmware-output-{}", Uuid::new_v4());
        state.selection = Some((selection_id.clone(), directory));
        Ok(Some(FirmwareOutputDirectorySelectionDto { selection_id }))
    }

    pub(crate) fn resolve(&self, selection_id: &str) -> Result<PathBuf, String> {
        self.state
            .lock()
            .expect("firmware output directory lock should not be poisoned")
            .selection
            .as_ref()
            .filter(|(stored_id, _)| !selection_id.is_empty() && stored_id == selection_id)
            .map(|(_, directory)| directory.clone())
            .ok_or_else(|| FIRMWARE_OUTPUT_DIRECTORY_SELECTION_ERROR.to_string())
    }

    pub(crate) fn clear(&self) {
        let mut state = self
            .state
            .lock()
            .expect("firmware output directory lock should not be poisoned");
        state.picker_epoch = state
            .picker_epoch
            .checked_add(1)
            .expect("firmware output directory picker epoch exhausted");
        state.selection.take();
    }
}

#[derive(Debug, Clone)]
struct RemoteFirmwareInspection {
    source: String,
    kind: RemoteFirmwareKind,
    entries: Vec<FirmwareExtractEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteFirmwareExtractionSelection {
    source: String,
    kind: RemoteFirmwareKind,
    entries: Vec<FirmwareExtractEntry>,
    total_bytes: u64,
}

struct RemoteFirmwareExtractionRequest {
    source: String,
    selected_ids: Vec<String>,
    output_directory: PathBuf,
}

fn build_remote_extraction_request(
    output_directories: &FirmwareOutputDirectoryRuntime,
    source: String,
    selected_ids: Vec<String>,
    output_directory_id: &str,
) -> Result<RemoteFirmwareExtractionRequest, String> {
    Ok(RemoteFirmwareExtractionRequest {
        source,
        selected_ids,
        output_directory: output_directories.resolve(output_directory_id)?,
    })
}

impl RemoteFirmwareInspectionRuntime {
    pub fn new() -> Self {
        Self::default()
    }

    fn replace(
        &self,
        source: String,
        kind: RemoteFirmwareKind,
        entries: Vec<FirmwareExtractEntry>,
    ) {
        *self
            .inspection
            .lock()
            .expect("remote firmware inspection lock should not be poisoned") =
            Some(RemoteFirmwareInspection {
                source,
                kind,
                entries,
            });
    }

    fn clear(&self) {
        self.inspection
            .lock()
            .expect("remote firmware inspection lock should not be poisoned")
            .take();
    }

    fn resolve_selected(
        &self,
        source: &str,
        selected_ids: &[String],
    ) -> Result<RemoteFirmwareExtractionSelection, String> {
        if selected_ids.is_empty() {
            return Err("请选择有效且不重复的远程固件分区。".to_string());
        }
        let inspection = self
            .inspection
            .lock()
            .expect("remote firmware inspection lock should not be poisoned")
            .clone()
            .ok_or_else(|| "远程固件分区列表已失效，请重新读取。".to_string())?;
        if inspection.source != source {
            return Err("远程固件地址已变化，请重新读取分区。".to_string());
        }
        let mut seen = HashSet::new();
        let mut entries = Vec::with_capacity(selected_ids.len());
        let mut total_bytes = 0u64;
        for id in selected_ids {
            if !seen.insert(id) {
                return Err("请选择有效且不重复的远程固件分区。".to_string());
            }
            let entry = inspection
                .entries
                .iter()
                .find(|entry| entry.id == *id)
                .ok_or_else(|| "请选择有效且不重复的远程固件分区。".to_string())?;
            entries.push(entry.clone());
            total_bytes = total_bytes.saturating_add(u64::try_from(entry.size_bytes).unwrap_or(0));
        }
        Ok(RemoteFirmwareExtractionSelection {
            source: inspection.source,
            kind: inspection.kind,
            entries,
            total_bytes,
        })
    }
}

fn remote_image_entries(members: Vec<ZipMember>) -> Vec<FirmwareExtractEntry> {
    let mut seen_names = HashSet::new();
    members
        .into_iter()
        .enumerate()
        .filter_map(|(index, member)| {
            let extension = Path::new(&member.full_name)
                .extension()
                .and_then(|extension| extension.to_str())?;
            if !extension.eq_ignore_ascii_case("img") && !extension.eq_ignore_ascii_case("bin") {
                return None;
            }
            if member.name.eq_ignore_ascii_case("payload") {
                return None;
            }
            if !seen_names.insert(member.name.clone()) {
                return None;
            }
            Some(FirmwareExtractEntry {
                id: index.to_string(),
                name: member.name,
                size_bytes: member.size_bytes,
            })
        })
        .collect()
}

fn remote_firmware_format(kind: RemoteFirmwareKind) -> &'static str {
    match kind {
        RemoteFirmwareKind::PayloadZip | RemoteFirmwareKind::PayloadRaw => "payload",
        RemoteFirmwareKind::DirectImageZip => "zip",
        RemoteFirmwareKind::Unsupported => "unknown",
    }
}

fn remote_error_to_domain(error: RemoteFirmwareError) -> nwflash_domain::DomainError {
    match error {
        RemoteFirmwareError::Cancelled => {
            nwflash_domain::DomainError::UserCancelled("远程固件操作已取消。".to_string())
        }
        RemoteFirmwareError::InvalidUrl(_) => nwflash_domain::DomainError::InvalidInput(
            "请输入有效的 HTTP 或 HTTPS 固件地址。".to_string(),
        ),
        RemoteFirmwareError::UnsupportedFormat => {
            nwflash_domain::DomainError::InvalidFormat("当前远程固件格式暂不支持。".to_string())
        }
        RemoteFirmwareError::RangeUnsupported => nwflash_domain::DomainError::RemoteApi(
            "远程服务器不支持分块读取，无法安全提取此固件。".to_string(),
        ),
        RemoteFirmwareError::Archive(_) => {
            nwflash_domain::DomainError::InvalidFormat("远程 ZIP 固件无法读取。".to_string())
        }
        RemoteFirmwareError::MissingPartition(_) => nwflash_domain::DomainError::InvalidInput(
            "所选远程固件分区不存在，请重新读取。".to_string(),
        ),
        RemoteFirmwareError::Integrity(_) => {
            nwflash_domain::DomainError::InvalidFormat("远程固件分区完整性校验失败。".to_string())
        }
        RemoteFirmwareError::Transport(_) => nwflash_domain::DomainError::RemoteApi(
            "远程固件读取失败，请检查地址和网络。".to_string(),
        ),
    }
}

fn remote_payload_application_error_to_domain(
    error: FirmwareExtractApplicationError,
) -> nwflash_domain::DomainError {
    match error {
        FirmwareExtractApplicationError::Canceled => {
            nwflash_domain::DomainError::UserCancelled("远程 payload 操作已取消。".to_string())
        }
        FirmwareExtractApplicationError::InvalidSelection => {
            nwflash_domain::DomainError::InvalidInput(
                "所选远程 payload 分区无效，请重新读取。".to_string(),
            )
        }
        FirmwareExtractApplicationError::UnsupportedFormat => {
            nwflash_domain::DomainError::InvalidFormat("远程 payload 格式不受支持。".to_string())
        }
        FirmwareExtractApplicationError::Vivo(_)
        | FirmwareExtractApplicationError::Format(_)
        | FirmwareExtractApplicationError::Directory(_)
        | FirmwareExtractApplicationError::Zip(_) => nwflash_domain::DomainError::InvalidOperation(
            "远程 payload 处理失败，请检查固件后重试。".to_string(),
        ),
    }
}

async fn inspect_local_or_payload(
    coordinator: nwflash_application::OperationCoordinator,
    payload_runtime: PayloadInspectionRuntime,
    provisioner: nwflash_infrastructure::PayloadDumperProvisioner,
    source_path: PathBuf,
) -> Result<FirmwareInspectionDto, String> {
    payload_runtime.clear();
    let format = task::spawn_blocking({
        let source_path = source_path.clone();
        move || nwflash_infrastructure::FirmwareFormatDetector::detect_local(&source_path)
    })
    .await
    .map_err(|error| format!("固件检查调度失败：{error}"))?
    .map_err(|error| error.to_string())?;
    let payload_zip = if format == nwflash_infrastructure::FirmwareFormat::Zip {
        task::spawn_blocking({
            let source_path = source_path.clone();
            move || {
                nwflash_infrastructure::FirmwarePackageInspector::contains_payload_bin(&source_path)
            }
        })
        .await
        .map_err(|error| format!("固件检查调度失败：{error}"))?
        .map_err(|error| error.to_string())?
    } else {
        false
    };
    if format == nwflash_infrastructure::FirmwareFormat::Payload || payload_zip {
        return inspect_payload_with_provisioner_operation(
            coordinator,
            payload_runtime,
            provisioner,
            source_path.to_string_lossy().into_owned(),
        )
        .await;
    }

    let inspection = Arc::new(Mutex::new(None));
    let inspection_for_operation = inspection.clone();
    coordinator
        .run_async(
            nwflash_domain::OperationKind::Hashing,
            "检查本地固件",
            move |context, cancellation| async move {
                context.report_stage("正在检查本地固件");
                let inspect_cancellation = cancellation.clone();
                let inspection_result = task::spawn_blocking(move || {
                    if inspect_cancellation.is_cancelled() {
                        return Err(FirmwareExtractApplicationError::Canceled);
                    }
                    FirmwareExtractService::inspect_local(&source_path)
                })
                .await
                .map_err(|error| {
                    nwflash_domain::DomainError::Internal(format!("固件检查调度失败：{error}"))
                })?;
                let inspection_result = inspection_result.map_err(application_error_to_domain)?;
                context.report_stage(format!(
                    "本地固件检查完成：发现 {} 个分区",
                    inspection_result.entries.len()
                ));
                context.report_progress(1.0);
                *inspection_for_operation.lock().map_err(|_| {
                    nwflash_domain::DomainError::Internal("固件检查结果锁不可用。".to_string())
                })? = Some(inspection_result);
                Ok(())
            },
        )
        .await
        .map_err(|error| error.to_string())?;

    let result = inspection
        .lock()
        .map_err(|_| "固件检查结果锁不可用。".to_string())?
        .take()
        .ok_or_else(|| "固件检查未产生结果。".to_string())?;
    Ok(firmware_inspection_dto(result))
}

struct RemoteZipProgressTracker {
    total_bytes: u64,
    current_partition: String,
    current_bytes: u64,
    completed_bytes: u64,
}

impl RemoteZipProgressTracker {
    fn new(total_bytes: u64) -> Self {
        Self {
            total_bytes,
            current_partition: String::new(),
            current_bytes: 0,
            completed_bytes: 0,
        }
    }

    fn record(&mut self, partition: &str, bytes: u64) -> u64 {
        if self.current_partition != partition {
            self.completed_bytes = self
                .completed_bytes
                .saturating_add(self.current_bytes)
                .min(self.total_bytes);
            self.current_partition = partition.to_string();
            self.current_bytes = 0;
        }
        self.current_bytes = self.current_bytes.max(bytes);
        self.completed_bytes
            .saturating_add(self.current_bytes)
            .min(self.total_bytes)
    }
}

fn remote_inspection_dto(
    kind: RemoteFirmwareKind,
    entries: Vec<FirmwareExtractEntry>,
) -> FirmwareInspectionDto {
    FirmwareInspectionDto {
        format: remote_firmware_format(kind).to_string(),
        entries: entries
            .into_iter()
            .map(|entry| FirmwareExtractEntryDto {
                id: entry.id,
                name: entry.name,
                size_bytes: entry.size_bytes,
            })
            .collect(),
    }
}

async fn inspect_remote_firmware_operation(
    coordinator: nwflash_application::OperationCoordinator,
    remote_runtime: RemoteFirmwareInspectionRuntime,
    provisioner: nwflash_infrastructure::PayloadDumperProvisioner,
    source: String,
    progress: FirmwareProgressReporter,
) -> Result<FirmwareInspectionDto, String> {
    remote_runtime.clear();
    let inspection = Arc::new(Mutex::new(None));
    let inspection_for_operation = inspection.clone();
    coordinator
        .run_async(
            nwflash_domain::OperationKind::Hashing,
            "检查远程固件",
            move |context, cancellation| async move {
                context.report_stage("正在检查远程固件格式");
                let probe_url = source.clone();
                let probe_cancellation = cancellation.clone();
                let kind = task::spawn_blocking(move || {
                    let mut is_canceled = || probe_cancellation.is_cancelled();
                    probe_remote_kind(&probe_url, None, &mut is_canceled)
                        .map_err(remote_error_to_domain)
                })
                .await
                .map_err(|error| {
                    nwflash_domain::DomainError::Internal(format!(
                        "远程固件格式检查调度失败：{error}"
                    ))
                })??;

                let (entries, format) = match kind {
                    RemoteFirmwareKind::DirectImageZip => {
                        context.report_stage("正在读取远程镜像分区");
                        let list_url = source.clone();
                        let list_cancellation = cancellation.clone();
                        let members = task::spawn_blocking(move || {
                            let mut is_canceled = || list_cancellation.is_cancelled();
                            list_zip_members(&list_url, None, &mut is_canceled)
                                .map_err(remote_error_to_domain)
                        })
                        .await
                        .map_err(|error| {
                            nwflash_domain::DomainError::Internal(format!(
                                "远程镜像列表读取调度失败：{error}"
                            ))
                        })??;
                        let entries = remote_image_entries(members);
                        if entries.is_empty() {
                            return Err(nwflash_domain::DomainError::InvalidFormat(
                                "远程 ZIP 中没有可提取的 img 或 bin 镜像。".to_string(),
                            ));
                        }
                        (entries, kind)
                    }
                    RemoteFirmwareKind::PayloadZip | RemoteFirmwareKind::PayloadRaw => {
                        context.report_stage("正在准备 payload 提取工具");
                        let executable = provisioner
                            .ensure_installed(&cancellation, None)
                            .await
                            .map_err(|error| {
                                nwflash_domain::DomainError::ExternalTool(format!(
                                    "payload 提取工具未就绪：{error}"
                                ))
                            })?;
                        context.report_stage("正在读取 payload 分区列表");
                        let source_path = source.clone();
                        let inspect_cancellation = cancellation.clone();
                        let metadata_result = task::spawn_blocking(move || {
                            let metadata_root = create_unique_firmware_root("firmware-metadata")
                                .map_err(FirmwareExtractApplicationError::Format)?;
                            let result = FirmwareExtractService::inspect_payload(
                                &executable,
                                &source_path,
                                metadata_root.as_path(),
                                || inspect_cancellation.is_cancelled(),
                            );
                            let _ = fs::remove_dir_all(metadata_root.as_path());
                            result
                        })
                        .await
                        .map_err(|error| {
                            nwflash_domain::DomainError::Internal(format!(
                                "远程 payload 元数据读取调度失败：{error}"
                            ))
                        });
                        let inspected = metadata_result
                            .map_err(|error| {
                                nwflash_domain::DomainError::Internal(format!(
                                    "远程 payload 元数据读取调度失败：{error}"
                                ))
                            })?
                            .map_err(remote_payload_application_error_to_domain)?;
                        progress.report_terminal(None, 1, 1, None);
                        (inspected.entries, kind)
                    }
                    RemoteFirmwareKind::Unsupported => {
                        return Err(remote_error_to_domain(
                            RemoteFirmwareError::UnsupportedFormat,
                        ));
                    }
                };

                let dto = remote_inspection_dto(format, entries.clone());
                remote_runtime.replace(source.clone(), format, entries);
                *inspection_for_operation.lock().map_err(|_| {
                    nwflash_domain::DomainError::Internal("远程固件检查结果锁不可用。".to_string())
                })? = Some(dto);
                context.report_progress(1.0);
                Ok(())
            },
        )
        .await
        .map_err(|error| error.to_string())?;
    let result = inspection
        .lock()
        .map_err(|_| "远程固件检查结果锁不可用。".to_string())?
        .take()
        .ok_or_else(|| "远程固件检查未产生结果。".to_string());
    result
}

async fn extract_remote_firmware_operation(
    coordinator: nwflash_application::OperationCoordinator,
    extraction_runtime: FirmwareExtractionRuntime,
    remote_runtime: RemoteFirmwareInspectionRuntime,
    provisioner: nwflash_infrastructure::PayloadDumperProvisioner,
    request: RemoteFirmwareExtractionRequest,
    progress: FirmwareProgressReporter,
) -> Result<FirmwareExtractionDto, String> {
    let selection = remote_runtime.resolve_selected(&request.source, &request.selected_ids)?;
    let extracted = Arc::new(Mutex::new(None));
    let extracted_for_operation = extracted.clone();
    coordinator
        .run_async(
            nwflash_domain::OperationKind::Hashing,
            "提取远程固件",
            move |context, cancellation| async move {
                progress.set_total_partitions(selection.entries.len());
                let images = match selection.kind {
                    RemoteFirmwareKind::DirectImageZip => {
                        context.report_stage("正在按需提取远程镜像");
                        let wanted_names = selection
                            .entries
                            .iter()
                            .map(|entry| entry.name.clone())
                            .collect::<Vec<_>>();
                        let source = selection.source.clone();
                        let output_directory = request.output_directory.clone();
                        let extraction_progress = progress.clone();
                        let total_bytes = selection.total_bytes;
                        let extraction_cancellation = cancellation.clone();
                        let result = task::spawn_blocking(move || {
                            let wanted = wanted_names
                                .iter()
                                .map(String::as_str)
                                .collect::<Vec<_>>();
                            let mut is_canceled = || extraction_cancellation.is_cancelled();
                            let mut progress_tracker = RemoteZipProgressTracker::new(total_bytes);
                            let mut report_progress = |partition: &str, bytes: u64| {
                                let completed_bytes = progress_tracker.record(partition, bytes);
                                extraction_progress.report(
                                    Some(partition.to_string()),
                                    completed_bytes,
                                    total_bytes,
                                    None,
                                );
                            };
                            extract_zip_members(
                                &source,
                                None,
                                &wanted,
                                &output_directory,
                                &mut is_canceled,
                                &mut report_progress,
                            )
                            .map_err(remote_error_to_domain)
                        })
                        .await
                        .map_err(|error| {
                            nwflash_domain::DomainError::Internal(format!(
                                "远程镜像提取调度失败：{error}"
                            ))
                        })??;
                        if result.len() != request.selected_ids.len() {
                            return Err(nwflash_domain::DomainError::InvalidInput(
                                "所选远程固件分区无法提取，请重新读取。".to_string(),
                            ));
                        }
                        progress.report_terminal(None, total_bytes, total_bytes, None);
                        result
                            .into_iter()
                            .map(|image| nwflash_domain::FlashImageInfo {
                                path: image.output_path,
                                size_bytes: image.size_bytes,
                            })
                            .collect::<Vec<_>>()
                    }
                    RemoteFirmwareKind::PayloadZip | RemoteFirmwareKind::PayloadRaw => {
                        context.report_stage("正在准备 payload 提取工具");
                        let executable = provisioner
                            .ensure_installed(&cancellation, None)
                            .await
                            .map_err(|error| {
                                nwflash_domain::DomainError::ExternalTool(format!(
                                    "payload 提取工具未就绪：{error}"
                                ))
                            })?;
                        context.report_stage("正在按需提取远程 payload 分区");
                        let source_path = selection.source.clone();
                        let entries = selection.entries.clone();
                        let total_partitions = entries.len();
                        let output_directory = request.output_directory.clone();
                        let extraction_progress = progress.clone();
                        let total_bytes = selection.total_bytes.max(1);
                        let extraction_cancellation = cancellation.clone();
                        let result = task::spawn_blocking(move || {
                            FirmwareExtractService::extract_payload_with_expected_sizes_and_progress(
                                &executable,
                                &source_path,
                                &entries,
                                &output_directory,
                                || extraction_cancellation.is_cancelled(),
                                move |current_partition, bytes_completed| extraction_progress.report(
                                    current_partition,
                                    bytes_completed,
                                    total_bytes,
                                    None,
                                ),
                            )
                        })
                        .await
                        .map_err(|error| {
                            nwflash_domain::DomainError::Internal(format!(
                                "远程 payload 提取调度失败：{error}"
                            ))
                        });
                        let images = result
                            .map_err(|error| {
                                nwflash_domain::DomainError::Internal(format!(
                                    "远程 payload 提取调度失败：{error}"
                                ))
                            })?
                            .map_err(remote_payload_application_error_to_domain)?;
                        context.report_stage(format!(
                            "远程 payload 分区提取完成：成功读取 {}/{} 个分区",
                            images.len(),
                            total_partitions
                        ));
                        progress.report_terminal(None, total_bytes, total_bytes, None);
                        images
                    }
                    RemoteFirmwareKind::Unsupported => {
                        return Err(remote_error_to_domain(
                            RemoteFirmwareError::UnsupportedFormat,
                        ));
                    }
                };
                context.report_stage(format!(
                    "远程固件提取完成：成功读取 {}/{} 个分区",
                    images.len(),
                    progress.total_partitions()
                ));
                context.report_progress(1.0);
                *extracted_for_operation.lock().map_err(|_| {
                    nwflash_domain::DomainError::Internal(
                        "远程固件提取结果锁不可用。".to_string(),
                    )
                })? = Some(images);
                Ok(())
            },
        )
        .await
        .map_err(|error| error.to_string())?;
    let images = extracted
        .lock()
        .map_err(|_| "远程固件提取结果锁不可用。".to_string())?
        .take()
        .ok_or_else(|| "远程固件提取未产生结果。".to_string())?;
    let result_ids = extraction_runtime.replace(images.clone())?;
    Ok(firmware_extraction_dto(images, result_ids))
}

#[tauri::command]
pub async fn firmware_select_output_directory(
    app_handle: AppHandle,
    state: State<'_, AppState>,
) -> Result<Option<FirmwareOutputDirectorySelectionDto>, String> {
    let picker = state.firmware_output_directories.begin_selection();
    let selected = select_firmware_output_directory(&app_handle).await?;
    state
        .firmware_output_directories
        .publish_selection(picker, selected)
}

async fn select_firmware_output_directory(
    app_handle: &AppHandle,
) -> Result<Option<PathBuf>, String> {
    let (sender, receiver) = oneshot::channel();
    app_handle.dialog().file().pick_folder(move |selected| {
        let _ = sender.send(selected.map(|path| path.into_path()));
    });
    receiver
        .await
        .map_err(|_| "提取输出目录选择窗口已关闭。".to_string())?
        .transpose()
        .map_err(|_| "无法读取所选提取输出目录。".to_string())
}

#[tauri::command]
pub async fn firmware_inspect_remote(
    state: State<'_, AppState>,
    url: String,
) -> Result<FirmwareInspectionDto, String> {
    let source = url.trim().to_string();
    validate_http_url(&source)
        .map_err(remote_error_to_domain)
        .map_err(|error| error.to_string())?;
    state.firmware_extraction.clear();
    state.payload_inspection.clear();
    inspect_remote_firmware_operation(
        state.operation_coordinator.clone(),
        state.remote_firmware_inspection.clone(),
        default_payload_provisioner(),
        source,
        state.firmware_progress.start(),
    )
    .await
}

#[tauri::command]
pub async fn firmware_extract_remote(
    state: State<'_, AppState>,
    url: String,
    selected_ids: Vec<String>,
    output_directory_id: String,
) -> Result<FirmwareExtractionDto, String> {
    let source = url.trim().to_string();
    validate_http_url(&source)
        .map_err(remote_error_to_domain)
        .map_err(|error| error.to_string())?;
    let request = build_remote_extraction_request(
        &state.firmware_output_directories,
        source,
        selected_ids,
        &output_directory_id,
    )?;
    state.firmware_extraction.clear();
    extract_remote_firmware_operation(
        state.operation_coordinator.clone(),
        state.firmware_extraction.clone(),
        state.remote_firmware_inspection.clone(),
        default_payload_provisioner(),
        request,
        state.firmware_progress.start(),
    )
    .await
}

#[tauri::command]
pub async fn firmware_inspect_local(
    state: State<'_, AppState>,
    source_path: String,
) -> Result<FirmwareInspectionDto, String> {
    state.firmware_extraction.clear();
    state.remote_firmware_inspection.clear();
    inspect_local_or_payload(
        state.operation_coordinator.clone(),
        state.payload_inspection.clone(),
        default_payload_provisioner(),
        PathBuf::from(source_path),
    )
    .await
}

#[tauri::command]
pub async fn firmware_inspect_line_flash_package(
    state: State<'_, AppState>,
    package_path: String,
) -> Result<FirmwareInspectionDto, String> {
    let inspection = Arc::new(Mutex::new(None));
    let inspection_for_operation = inspection.clone();
    state
        .operation_coordinator
        .run_async(
            nwflash_domain::OperationKind::Hashing,
            "检查线刷固件包",
            move |context, cancellation| async move {
                context.report_stage("正在检查线刷固件包");
                let inspect_cancellation = cancellation.clone();
                let inspection_result = task::spawn_blocking(move || {
                    if inspect_cancellation.is_cancelled() {
                        return Err("线刷固件包检查已取消。".to_string());
                    }
                    line_flash_package_inspection(&PathBuf::from(package_path))
                })
                .await
                .map_err(|error| {
                    nwflash_domain::DomainError::Internal(format!("线刷固件检查调度失败：{error}"))
                })?;
                let inspection_result = inspection_result
                    .map_err(|error| nwflash_domain::DomainError::InvalidOperation(error))?;
                context.report_stage(format!(
                    "线刷固件包检查完成：发现 {} 个分区",
                    inspection_result.entries.len()
                ));
                context.report_progress(1.0);
                *inspection_for_operation.lock().map_err(|_| {
                    nwflash_domain::DomainError::Internal("线刷固件检查结果锁不可用。".to_string())
                })? = Some(inspection_result);
                Ok(())
            },
        )
        .await
        .map_err(|error| error.to_string())?;

    let result = inspection
        .lock()
        .map_err(|_| "线刷固件检查结果锁不可用。".to_string())?
        .take()
        .ok_or_else(|| "线刷固件检查未产生结果。".to_string())?;
    Ok(result)
}

#[tauri::command]
pub async fn firmware_extract_vivo_local(
    state: State<'_, AppState>,
    source_path: String,
    selected_ids: Vec<String>,
    output_directory_id: String,
) -> Result<FirmwareExtractionDto, String> {
    let output_directory = state
        .firmware_output_directories
        .resolve(&output_directory_id)?;
    state.firmware_extraction.clear();
    let progress = state.firmware_progress.start();
    let extracted = Arc::new(Mutex::new(None));
    let extracted_for_operation = extracted.clone();
    state
        .operation_coordinator
        .run_async(
            nwflash_domain::OperationKind::Hashing,
            "提取本地固件",
            move |context, cancellation| async move {
                let total_partitions = selected_ids.len();
                progress.set_total_partitions(total_partitions);
                context.report_stage("正在提取已选择的固件分区");
                let source_path = PathBuf::from(source_path);
                let progress_for_extraction = progress.clone();
                let extraction = task::spawn_blocking(move || {
                    FirmwareExtractService::extract_local_with_cancel_and_progress(
                        &source_path,
                        &selected_ids,
                        &output_directory,
                        || cancellation.is_cancelled(),
                        move |update| {
                            let current_partition = Path::new(&update.current_entry)
                                .file_name()
                                .and_then(|name| name.to_str())
                                .map(str::to_string);
                            if update.current_entry.is_empty()
                                && update.completed_bytes == update.total_bytes
                            {
                                progress_for_extraction.report_terminal(
                                    current_partition,
                                    update.completed_bytes,
                                    update.total_bytes,
                                    Some(update.gzip_stream_bytes),
                                );
                            } else {
                                progress_for_extraction.report(
                                    current_partition,
                                    update.completed_bytes,
                                    update.total_bytes,
                                    Some(update.gzip_stream_bytes),
                                );
                            }
                        },
                    )
                })
                .await
                .map_err(|error| {
                    nwflash_domain::DomainError::Internal(format!("固件提取调度失败：{error}"))
                })?;
                let images = extraction.map_err(application_error_to_domain)?;
                progress.report_terminal(None, 0, 0, None);
                context.report_stage(format!(
                    "本地固件提取完成：成功读取 {}/{} 个分区",
                    images.len(),
                    total_partitions
                ));
                context.report_progress(1.0);
                *extracted_for_operation.lock().map_err(|_| {
                    nwflash_domain::DomainError::Internal("固件提取结果锁不可用。".to_string())
                })? = Some(images);
                Ok(())
            },
        )
        .await
        .map_err(|error| error.to_string())?;

    let images = extracted
        .lock()
        .map_err(|_| "固件提取结果锁不可用。".to_string())?
        .take()
        .ok_or_else(|| "固件提取未产生结果。".to_string())?;
    let result_ids = state.firmware_extraction.replace(images.clone())?;
    Ok(firmware_extraction_dto(images, result_ids))
}

async fn inspect_payload_with_provisioner_operation(
    coordinator: nwflash_application::OperationCoordinator,
    payload_runtime: PayloadInspectionRuntime,
    provisioner: nwflash_infrastructure::PayloadDumperProvisioner,
    source: String,
) -> Result<FirmwareInspectionDto, String> {
    payload_runtime.clear();
    let inspection = Arc::new(Mutex::new(None));
    let inspection_for_operation = inspection.clone();

    coordinator
        .run_async(
            nwflash_domain::OperationKind::Hashing,
            "读取 payload 分区",
            move |context, cancellation| async move {
                let source_for_inspection = source.clone();
                context.report_stage("正在准备 payload 提取工具");
                let executable = provisioner
                    .ensure_installed(&cancellation, None)
                    .await
                    .map_err(|error| {
                        nwflash_domain::DomainError::ExternalTool(format!(
                            "payload_dumper 未就绪：{error}"
                        ))
                    })?;
                if cancellation.is_cancelled() {
                    return Err(nwflash_domain::DomainError::UserCancelled(
                        "读取 payload 分区已取消。".to_string(),
                    ));
                }
                context.report_stage("正在读取 payload 分区列表");
                let inspection_result = task::spawn_blocking(move || {
                    let metadata_root = create_unique_firmware_root("firmware-metadata")
                        .map_err(FirmwareExtractApplicationError::Format)?;
                    let result = FirmwareExtractService::inspect_payload(
                        &executable,
                        &source_for_inspection,
                        metadata_root.as_path(),
                        || cancellation.is_cancelled(),
                    );
                    let _ = fs::remove_dir_all(metadata_root.as_path());
                    result
                })
                .await
                .map_err(|error| {
                    nwflash_domain::DomainError::Internal(format!(
                        "payload 元数据读取调度失败：{error}"
                    ))
                })?;
                let inspected = inspection_result.map_err(application_error_to_domain)?;
                payload_runtime.replace(source, inspected.entries.clone());
                context.report_progress(1.0);
                *inspection_for_operation.lock().map_err(|_| {
                    nwflash_domain::DomainError::Internal("payload 分区结果锁不可用。".to_string())
                })? = Some(inspected);
                Ok(())
            },
        )
        .await
        .map_err(|error| error.to_string())?;

    let result = inspection
        .lock()
        .map_err(|_| "payload 分区结果锁不可用。".to_string())?
        .take()
        .map(firmware_inspection_dto)
        .ok_or_else(|| "payload 未产生分区元数据。".to_string());
    result
}

#[tauri::command]
pub async fn firmware_inspect_payload_local(
    state: State<'_, AppState>,
    source_path: String,
) -> Result<FirmwareInspectionDto, String> {
    state.firmware_extraction.clear();
    state.payload_inspection.clear();
    state.remote_firmware_inspection.clear();
    inspect_payload_with_provisioner_operation(
        state.operation_coordinator.clone(),
        state.payload_inspection.clone(),
        default_payload_provisioner(),
        source_path,
    )
    .await
}

async fn extract_payload_with_provisioner_operation(
    coordinator: nwflash_application::OperationCoordinator,
    extraction_runtime: FirmwareExtractionRuntime,
    payload_runtime: PayloadInspectionRuntime,
    provisioner: nwflash_infrastructure::PayloadDumperProvisioner,
    selected_ids: Vec<String>,
    output_directory: PathBuf,
    progress: Option<FirmwareProgressReporter>,
) -> Result<FirmwareExtractionDto, String> {
    let selection = payload_runtime.resolve_selected(&selected_ids)?;
    let extracted = Arc::new(Mutex::new(None));
    let extracted_for_operation = extracted.clone();

    coordinator
        .run_async(
            nwflash_domain::OperationKind::Hashing,
            "提取 payload 分区",
            move |context, cancellation| async move {
                let total_partitions = selection.entries.len();
                if let Some(progress) = &progress {
                    progress.set_total_partitions(total_partitions);
                }
                context.report_stage("正在准备 payload 提取工具");
                let executable = provisioner
                    .ensure_installed(&cancellation, None)
                    .await
                    .map_err(|error| {
                        nwflash_domain::DomainError::ExternalTool(format!(
                            "payload_dumper 未就绪：{error}"
                        ))
                    })?;
                if cancellation.is_cancelled() {
                    return Err(nwflash_domain::DomainError::UserCancelled(
                        "提取 payload 分区已取消。".to_string(),
                    ));
                }
                context.report_stage("正在提取已选择的 payload 分区");
                let total_bytes = selection.total_bytes;
                let progress_for_extraction = progress.clone();
                let extraction = task::spawn_blocking(move || {
                    FirmwareExtractService::extract_payload_with_expected_sizes_and_progress(
                        &executable,
                        &selection.source,
                        &selection.entries,
                        &output_directory,
                        || cancellation.is_cancelled(),
                        move |current_partition, bytes_completed| {
                            if let Some(progress) = &progress_for_extraction {
                                progress.report(
                                    current_partition,
                                    bytes_completed,
                                    total_bytes,
                                    None,
                                );
                            }
                        },
                    )
                })
                .await
                .map_err(|error| {
                    nwflash_domain::DomainError::Internal(format!("payload 提取调度失败：{error}"))
                })?;
                let images = extraction.map_err(application_error_to_domain)?;
                context.report_stage(format!(
                    "payload 分区提取完成：成功读取 {}/{} 个分区",
                    images.len(),
                    total_partitions
                ));
                if let Some(progress) = &progress {
                    progress.report_terminal(None, total_bytes, total_bytes, None);
                }
                context.report_progress(1.0);
                *extracted_for_operation.lock().map_err(|_| {
                    nwflash_domain::DomainError::Internal("payload 提取结果锁不可用。".to_string())
                })? = Some(images);
                Ok(())
            },
        )
        .await
        .map_err(|error| error.to_string())?;

    let images = extracted
        .lock()
        .map_err(|_| "payload 提取结果锁不可用。".to_string())?
        .take()
        .ok_or_else(|| "payload 提取未产生结果。".to_string())?;
    let result_ids = extraction_runtime.replace(images.clone())?;
    Ok(firmware_extraction_dto(images, result_ids))
}

#[tauri::command]
pub async fn firmware_extract_payload_local(
    state: State<'_, AppState>,
    selected_ids: Vec<String>,
    output_directory_id: String,
) -> Result<FirmwareExtractionDto, String> {
    let output_directory = state
        .firmware_output_directories
        .resolve(&output_directory_id)?;
    state.firmware_extraction.clear();
    extract_payload_with_provisioner_operation(
        state.operation_coordinator.clone(),
        state.firmware_extraction.clone(),
        state.payload_inspection.clone(),
        default_payload_provisioner(),
        selected_ids,
        output_directory,
        Some(state.firmware_progress.start()),
    )
    .await
}

fn firmware_inspection_dto(inspection: FirmwareExtractInspection) -> FirmwareInspectionDto {
    FirmwareInspectionDto {
        format: match inspection.format {
            nwflash_infrastructure::FirmwareFormat::ImageDirectory => "imageDirectory",
            nwflash_infrastructure::FirmwareFormat::VivoGzipTar => "vivoGzipTar",
            nwflash_infrastructure::FirmwareFormat::Zip => "zip",
            nwflash_infrastructure::FirmwareFormat::Payload => "payload",
            nwflash_infrastructure::FirmwareFormat::Unknown => "unknown",
        }
        .to_string(),
        entries: inspection
            .entries
            .into_iter()
            .map(|entry| FirmwareExtractEntryDto {
                id: entry.id,
                name: entry.name,
                size_bytes: entry.size_bytes,
            })
            .collect(),
    }
}

fn firmware_extraction_dto(
    images: Vec<nwflash_domain::FlashImageInfo>,
    result_ids: Vec<Option<String>>,
) -> FirmwareExtractionDto {
    FirmwareExtractionDto {
        images: images
            .into_iter()
            .zip(result_ids)
            .map(|(image, result_id)| ExtractedFirmwareImageDto {
                name: Path::new(&image.path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("镜像")
                    .to_string(),
                size_bytes: image.size_bytes,
                result_id,
            })
            .collect(),
    }
}

fn line_flash_package_inspection(package_path: &Path) -> Result<FirmwareInspectionDto, String> {
    FirmwareExtractService::inspect_line_flash_package(package_path)
        .map(firmware_inspection_dto)
        .map_err(|error| error.to_string())
}

#[cfg(test)]
fn line_flash_package_extraction(
    package_path: &Path,
    selected_id: &str,
    staging_root: &Path,
) -> Result<FirmwareExtractionDto, String> {
    let inspection = FirmwareExtractService::inspect_line_flash_package(package_path)
        .map_err(|error| error.to_string())?;
    let name = selected_id
        .parse::<usize>()
        .ok()
        .and_then(|index| inspection.entries.get(index))
        .map(|entry| entry.name.clone())
        .ok_or_else(|| "请选择有效且不重复的固件分区。".to_string())?;
    let image =
        FirmwareExtractService::extract_line_flash_package(package_path, selected_id, staging_root)
            .map_err(|error| error.to_string())?;
    Ok(FirmwareExtractionDto {
        images: vec![ExtractedFirmwareImageDto {
            name,
            size_bytes: image.size_bytes,
            result_id: None,
        }],
    })
}

#[cfg(test)]
fn prepare_line_flash_artifact(
    runtime: &FirmwareArtifactRuntime,
    package_path: &Path,
    selected_id: &str,
    staging_root: &Path,
) -> Result<PreparedFirmwareArtifactDto, FirmwareArtifactPreparationError> {
    prepare_line_flash_artifact_with_cancel(
        runtime,
        package_path,
        selected_id,
        staging_root,
        || false,
    )
}

fn prepare_line_flash_artifact_with_cancel<F>(
    runtime: &FirmwareArtifactRuntime,
    package_path: &Path,
    selected_id: &str,
    staging_root: &Path,
    is_canceled: F,
) -> Result<PreparedFirmwareArtifactDto, FirmwareArtifactPreparationError>
where
    F: FnMut() -> bool,
{
    let inspection = FirmwareExtractService::inspect_line_flash_package(package_path)
        .map_err(FirmwareArtifactPreparationError::Application)?;
    let name = selected_id
        .parse::<usize>()
        .ok()
        .and_then(|index| inspection.entries.get(index))
        .map(|entry| entry.name.clone())
        .ok_or(FirmwareArtifactPreparationError::Application(
            FirmwareExtractApplicationError::InvalidSelection,
        ))?;
    let partition = quick_flash_partition_from_name(&name)
        .map_err(FirmwareArtifactPreparationError::InvalidPartition)?;
    let image = FirmwareExtractService::extract_line_flash_package_with_cancel(
        package_path,
        selected_id,
        staging_root,
        is_canceled,
    )
    .map_err(FirmwareArtifactPreparationError::Application)?;
    let artifact_id = runtime.replace(partition, image.clone(), staging_root.to_path_buf());
    Ok(PreparedFirmwareArtifactDto {
        artifact_id,
        name,
        size_bytes: image.size_bytes,
    })
}

fn stage_extracted_firmware_artifact<F>(
    runtime: &FirmwareArtifactRuntime,
    result: FirmwareExtractionResult,
    staging_root: InternalFirmwareStagingRoot,
    mut is_canceled: F,
) -> Result<PreparedFirmwareArtifactDto, FirmwareArtifactPreparationError>
where
    F: FnMut() -> bool,
{
    let extension = flashable_image_extension(Path::new(&result.name)).ok_or_else(|| {
        FirmwareArtifactPreparationError::InvalidPartition(
            "该镜像不在受控的快速刷写分区范围内。".to_string(),
        )
    })?;
    let staged_path = staging_root.as_path().join(format!(
        "{}-{}.{}",
        result.partition.partition_name(),
        unique_firmware_suffix(),
        extension
    ));
    let copy_result = copy_image_with_cancel(
        Path::new(&result.image.path),
        &staged_path,
        result.image.size_bytes,
        &mut is_canceled,
    );
    if let Err(error) = copy_result {
        let _ = fs::remove_file(&staged_path);
        let _ = fs::remove_dir_all(staging_root.as_path());
        return Err(error);
    }
    let staged = nwflash_domain::FlashImageInfo {
        path: staged_path.to_string_lossy().into_owned(),
        size_bytes: result.image.size_bytes,
    };
    let artifact_id = runtime.replace_owned(result.partition, staged, staging_root);
    Ok(PreparedFirmwareArtifactDto {
        artifact_id,
        name: result.name,
        size_bytes: result.image.size_bytes,
    })
}

#[derive(Debug)]
enum FirmwareArtifactPreparationError {
    Application(FirmwareExtractApplicationError),
    Staging(String),
    InvalidPartition(String),
}

impl fmt::Display for FirmwareArtifactPreparationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Application(error) => error.fmt(formatter),
            Self::Staging(message) | Self::InvalidPartition(message) => {
                formatter.write_str(message)
            }
        }
    }
}

async fn prepare_line_flash_artifact_operation(
    coordinator: nwflash_application::OperationCoordinator,
    runtime: FirmwareArtifactRuntime,
    package_path: PathBuf,
    selected_id: String,
) -> Result<PreparedFirmwareArtifactDto, String> {
    let prepared = Arc::new(Mutex::new(None));
    let prepared_for_operation = prepared.clone();

    coordinator
        .run_async(
            nwflash_domain::OperationKind::Hashing,
            "准备线刷固件",
            move |context, cancellation| async move {
                context.report_stage("正在提取已选择的线刷分区");
                let runtime = runtime.clone();
                let extraction = task::spawn_blocking(move || {
                    let staging_root = create_unique_firmware_root("firmware-stage")
                        .map_err(FirmwareArtifactPreparationError::Staging)?;
                    let result = prepare_line_flash_artifact_with_cancel(
                        &runtime,
                        &package_path,
                        &selected_id,
                        staging_root.as_path(),
                        || cancellation.is_cancelled(),
                    );
                    match result {
                        Ok(prepared) => {
                            let artifact = runtime
                                .get(&prepared.artifact_id)
                                .map_err(FirmwareArtifactPreparationError::Staging)?;
                            let artifact_id = runtime.replace_owned(
                                artifact.partition,
                                artifact.image,
                                staging_root,
                            );
                            Ok(PreparedFirmwareArtifactDto {
                                artifact_id,
                                name: prepared.name,
                                size_bytes: prepared.size_bytes,
                            })
                        }
                        Err(error) => {
                            let _ = fs::remove_dir_all(staging_root.as_path());
                            Err(error)
                        }
                    }
                })
                .await
                .map_err(|error| {
                    nwflash_domain::DomainError::Internal(format!("线刷固件提取调度失败：{error}"))
                })?;
                let artifact = extraction.map_err(preparation_error_to_domain)?;
                context.report_progress(1.0);
                *prepared_for_operation.lock().map_err(|_| {
                    nwflash_domain::DomainError::Internal("固件工件结果锁不可用。".to_string())
                })? = Some(artifact);
                Ok(())
            },
        )
        .await
        .map_err(|error| error.to_string())?;

    let result = prepared
        .lock()
        .map_err(|_| "固件工件结果锁不可用。".to_string())?
        .take()
        .ok_or_else(|| "固件提取未产生结果。".to_string());
    result
}

#[tauri::command]
pub async fn firmware_prepare_line_flash_artifact(
    state: State<'_, AppState>,
    package_path: String,
    selected_id: String,
) -> Result<PreparedFirmwareArtifactDto, String> {
    prepare_line_flash_artifact_operation(
        state.operation_coordinator.clone(),
        state.firmware_artifacts.clone(),
        PathBuf::from(package_path),
        selected_id,
    )
    .await
}

async fn prepare_extracted_artifact_operation(
    coordinator: nwflash_application::OperationCoordinator,
    artifacts: FirmwareArtifactRuntime,
    result: FirmwareExtractionResult,
) -> Result<PreparedFirmwareArtifactDto, String> {
    let prepared = Arc::new(Mutex::new(None));
    let prepared_for_operation = prepared.clone();
    coordinator
        .run_async(
            nwflash_domain::OperationKind::Hashing,
            "准备提取固件",
            move |context, cancellation| async move {
                context.report_stage("正在准备受控快速刷写镜像");
                let prepared_artifact = task::spawn_blocking(move || {
                    let staging_root = create_unique_firmware_root("firmware-stage")
                        .map_err(FirmwareArtifactPreparationError::Staging)?;
                    stage_extracted_firmware_artifact(&artifacts, result, staging_root, || {
                        cancellation.is_cancelled()
                    })
                })
                .await
                .map_err(|error| {
                    nwflash_domain::DomainError::Internal(format!("提取固件准备调度失败：{error}"))
                })?
                .map_err(preparation_error_to_domain)?;
                context.report_progress(1.0);
                *prepared_for_operation.lock().map_err(|_| {
                    nwflash_domain::DomainError::Internal("固件工件结果锁不可用。".to_string())
                })? = Some(prepared_artifact);
                Ok(())
            },
        )
        .await
        .map_err(|error| error.to_string())?;

    let result = prepared
        .lock()
        .map_err(|_| "固件工件结果锁不可用。".to_string())?
        .take()
        .ok_or_else(|| "固件提取未产生工件。".to_string());
    result
}

#[tauri::command]
pub async fn firmware_prepare_extracted_artifact(
    state: State<'_, AppState>,
    result_id: String,
) -> Result<PreparedFirmwareArtifactDto, String> {
    let result = state.firmware_extraction.get(&result_id)?;
    prepare_extracted_artifact_operation(
        state.operation_coordinator.clone(),
        state.firmware_artifacts.clone(),
        result,
    )
    .await
}

fn quick_flash_partition_from_name(
    name: &str,
) -> Result<nwflash_domain::QuickFlashPartition, String> {
    match Path::new(name)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "boot" => Ok(nwflash_domain::QuickFlashPartition::Boot),
        "init_boot" => Ok(nwflash_domain::QuickFlashPartition::InitBoot),
        "vendor_boot" => Ok(nwflash_domain::QuickFlashPartition::VendorBoot),
        "lk" => Ok(nwflash_domain::QuickFlashPartition::Lk),
        _ => Err("该镜像不在受控的快速刷写分区范围内。".to_string()),
    }
}

fn flashable_image_extension(path: &Path) -> Option<&str> {
    path.extension()
        .and_then(|extension| extension.to_str())
        .filter(|extension| {
            extension.eq_ignore_ascii_case("img") || extension.eq_ignore_ascii_case("bin")
        })
}

fn copy_image_snapshot(
    source: &Path,
    destination: &Path,
    expected_size: i64,
) -> Result<(), String> {
    copy_image_bytes(source, destination, expected_size, || false)
        .map_err(|error| error.to_string())
}

fn copy_image_with_cancel(
    source: &Path,
    destination: &Path,
    expected_size: i64,
    is_canceled: &mut impl FnMut() -> bool,
) -> Result<(), FirmwareArtifactPreparationError> {
    copy_image_bytes(source, destination, expected_size, is_canceled)
}

fn copy_image_bytes(
    source: &Path,
    destination: &Path,
    expected_size: i64,
    mut is_canceled: impl FnMut() -> bool,
) -> Result<(), FirmwareArtifactPreparationError> {
    let result = (|| {
        let mut input = File::open(source).map_err(|error| {
            FirmwareArtifactPreparationError::Staging(format!("无法读取固件提取结果：{error}"))
        })?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination)
            .map_err(|error| {
                FirmwareArtifactPreparationError::Staging(format!("无法创建固件临时文件：{error}"))
            })?;
        let mut buffer = [0u8; 8192];
        let mut bytes = 0i64;
        loop {
            if is_canceled() {
                return Err(FirmwareArtifactPreparationError::Application(
                    FirmwareExtractApplicationError::Canceled,
                ));
            }
            let count = input.read(&mut buffer).map_err(|error| {
                FirmwareArtifactPreparationError::Staging(format!("无法读取固件提取结果：{error}"))
            })?;
            if count == 0 {
                break;
            }
            output.write_all(&buffer[..count]).map_err(|error| {
                FirmwareArtifactPreparationError::Staging(format!("无法写入固件临时文件：{error}"))
            })?;
            bytes = bytes.saturating_add(i64::try_from(count).unwrap_or(i64::MAX));
        }
        output.sync_all().map_err(|error| {
            FirmwareArtifactPreparationError::Staging(format!("无法完成固件临时文件：{error}"))
        })?;
        if bytes <= 0 || bytes != expected_size {
            return Err(FirmwareArtifactPreparationError::Staging(
                "固件提取结果大小已变化，请重新提取。".to_string(),
            ));
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(destination);
    }
    result
}

fn create_unique_firmware_root(namespace: &str) -> Result<InternalFirmwareStagingRoot, String> {
    let base = std::env::temp_dir().join("nwflash").join(namespace);
    fs::create_dir_all(&base).map_err(|error| format!("无法创建固件临时目录：{error}"))?;
    let timestamp = unique_firmware_suffix();

    for attempt in 0..16 {
        let candidate = base.join(format!("{}-{timestamp}-{attempt}", std::process::id()));
        match fs::create_dir(&candidate) {
            Ok(()) => return Ok(InternalFirmwareStagingRoot(candidate)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("无法创建固件临时目录：{error}")),
        }
    }

    Err("无法创建唯一的固件临时目录。".to_string())
}

fn unique_firmware_suffix() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}

fn default_payload_provisioner() -> nwflash_infrastructure::PayloadDumperProvisioner {
    nwflash_infrastructure::PayloadDumperProvisioner::bundled(
        nwflash_windows::bundled_resource_root(),
    )
}

fn application_error_to_domain(
    error: FirmwareExtractApplicationError,
) -> nwflash_domain::DomainError {
    match error {
        FirmwareExtractApplicationError::Canceled => {
            nwflash_domain::DomainError::UserCancelled(error.to_string())
        }
        error => nwflash_domain::DomainError::InvalidOperation(error.to_string()),
    }
}

fn preparation_error_to_domain(
    error: FirmwareArtifactPreparationError,
) -> nwflash_domain::DomainError {
    match error {
        FirmwareArtifactPreparationError::Application(
            FirmwareExtractApplicationError::Canceled,
        ) => nwflash_domain::DomainError::UserCancelled(error.to_string()),
        error => nwflash_domain::DomainError::InvalidOperation(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs::{self, File},
        io::{Read, Write},
        net::{TcpListener, TcpStream},
        sync::Arc,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;
    use nwflash_application::{FirmwareExtractEntry, FirmwareExtractInspection};
    use nwflash_infrastructure::{
        remote_firmware::{RemoteFirmwareKind, ZipMember},
        FirmwareFormat,
    };
    use sha2::{Digest, Sha256};
    use zip4::{write::SimpleFileOptions, ZipWriter};

    fn temp_root(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be available")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("nwflash-{label}-{nonce}"));
        fs::create_dir_all(&root).expect("fixture directory should be created");
        root
    }

    fn write_image_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let file = File::create(path).expect("zip fixture should be created");
        let mut archive = ZipWriter::new(file);
        for (entry, data) in entries {
            archive
                .start_file(*entry, SimpleFileOptions::default())
                .expect("image should be added");
            archive.write_all(data).expect("image should be written");
        }
        archive.finish().expect("zip fixture should be finalized");
    }

    fn write_metadata_tool(root: &Path) -> PathBuf {
        let executable = root.join("payload_dumper.cmd");
        fs::write(
            &executable,
            "@echo off\r\nset output=\r\n:next\r\nif \"%~1\"==\"\" goto done\r\nif \"%~1\"==\"-o\" set output=%~2\r\nshift\r\ngoto next\r\n:done\r\n>\"%output%\\metadata.json\" echo {\"partitions\":[{\"partition_name\":\"boot\",\"size_in_bytes\":4,\"compression_type\":\"none\"}]}\r\nexit /b 0\r\n",
        )
        .expect("payload tool script should be written");
        executable
    }

    fn test_provisioner(
        root: &Path,
        executable: PathBuf,
    ) -> nwflash_infrastructure::PayloadDumperProvisioner {
        let executable_hash = format!(
            "{:x}",
            Sha256::digest(fs::read(&executable).expect("test executable should be readable"),)
        );
        nwflash_infrastructure::PayloadDumperProvisioner::with_expected_sha256(
            nwflash_infrastructure::RemoteAssetDownloader::default(),
            Some(root.join("cache")),
            Some(executable),
            executable_hash,
        )
    }

    fn spawn_remote_payload_server(body: Vec<u8>) -> String {
        let body = Arc::new(body);
        let listener = TcpListener::bind("127.0.0.1:0").expect("payload server should bind");
        let address = listener
            .local_addr()
            .expect("payload server address should be available");
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let body = body.clone();
                std::thread::spawn(move || {
                    let _ = serve_remote_payload_request(&mut stream, &body);
                });
            }
        });
        format!("http://{address}/payload.bin")
    }

    fn serve_remote_payload_request(stream: &mut TcpStream, body: &[u8]) -> std::io::Result<()> {
        let mut request = Vec::new();
        let mut buffer = [0u8; 4096];
        loop {
            let count = stream.read(&mut buffer)?;
            if count == 0 {
                return Ok(());
            }
            request.extend_from_slice(&buffer[..count]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        let request_text = String::from_utf8_lossy(&request);
        let is_head = request_text.starts_with("HEAD ");
        let range = request_text.lines().find_map(|line| {
            let lower = line.to_ascii_lowercase();
            lower
                .strip_prefix("range:")
                .and_then(|value| value.trim().strip_prefix("bytes="))
                .and_then(|value| {
                    let (start, end) = value.split_once('-')?;
                    Some((start.parse::<usize>().ok()?, end.parse::<usize>().ok()?))
                })
        });
        let total = body.len() as u64;
        if is_head {
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {total}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n"
            )?;
            return Ok(());
        }
        match range {
            Some((start, end)) if start <= end && end < body.len() => {
                let bytes = &body[start..=end];
                write!(
                    stream,
                    "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{end}/{total}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    bytes.len()
                )?;
                stream.write_all(bytes)?;
            }
            _ => {
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {total}\r\nConnection: close\r\n\r\n"
                )?;
                stream.write_all(body)?;
            }
        }
        Ok(())
    }

    fn write_recording_metadata_tool(root: &Path, record: &Path) -> PathBuf {
        let executable = root.join("payload_dumper.cmd");
        fs::write(
            &executable,
            format!(
                "@echo off\r\n>\"{}\" echo %~1\r\nset output=\r\n:next\r\nif \"%~1\"==\"\" goto done\r\nif \"%~1\"==\"-o\" set output=%~2\r\nshift\r\ngoto next\r\n:done\r\n>\"%output%\\metadata.json\" echo {{\"partitions\":[{{\"partition_name\":\"boot\",\"size_in_bytes\":9,\"compression_type\":\"none\"}}]}}\r\nexit /b 0\r\n",
                record.display()
            ),
        )
        .expect("recording payload tool should be written");
        executable
    }

    fn write_recording_extraction_tool(root: &Path, record: &Path) -> PathBuf {
        let executable = root.join("payload_dumper.cmd");
        fs::write(
            &executable,
            format!(
                "@echo off\r\n>\"{}\" echo %~1\r\nset output=\r\nset partitions=\r\n:next\r\nif \"%~1\"==\"\" goto done\r\nif \"%~1\"==\"-i\" set partitions=%~2\r\nif \"%~1\"==\"-o\" set output=%~2\r\nshift\r\ngoto next\r\n:done\r\nfor %%p in (%partitions:,= %) do >\"%output%\\%%p.img\" echo payload\r\nexit /b 0\r\n",
                record.display()
            ),
        )
        .expect("recording extraction tool should be written");
        executable
    }

    #[test]
    fn remote_image_entries_keep_only_img_and_bin_members() {
        let entries = remote_image_entries(vec![
            ZipMember {
                name: "boot".to_string(),
                full_name: "images/boot.img".to_string(),
                size_bytes: 4,
            },
            ZipMember {
                name: "payload".to_string(),
                full_name: "payload.bin".to_string(),
                size_bytes: 8,
            },
            ZipMember {
                name: "system.new.dat.0".to_string(),
                full_name: "system.new.dat.0".to_string(),
                size_bytes: 12,
            },
            ZipMember {
                name: "vbmeta".to_string(),
                full_name: "vbmeta.bin".to_string(),
                size_bytes: 2,
            },
        ]);

        assert_eq!(
            entries,
            vec![
                FirmwareExtractEntry {
                    id: "0".to_string(),
                    name: "boot".to_string(),
                    size_bytes: 4,
                },
                FirmwareExtractEntry {
                    id: "3".to_string(),
                    name: "vbmeta".to_string(),
                    size_bytes: 2,
                },
            ]
        );
    }

    #[test]
    fn remote_inspection_requires_the_same_checked_url_and_valid_ids() {
        let runtime = RemoteFirmwareInspectionRuntime::new();
        runtime.replace(
            "https://firmware.example.test/ota.zip?token=secret".to_string(),
            RemoteFirmwareKind::DirectImageZip,
            vec![FirmwareExtractEntry {
                id: "0".to_string(),
                name: "boot".to_string(),
                size_bytes: 4,
            }],
        );

        assert!(runtime
            .resolve_selected(
                "https://firmware.example.test/other.zip",
                &["0".to_string()]
            )
            .is_err());
        assert!(runtime
            .resolve_selected(
                "https://firmware.example.test/ota.zip?token=secret",
                &["unknown".to_string()]
            )
            .is_err());
        assert_eq!(
            runtime
                .resolve_selected(
                    "https://firmware.example.test/ota.zip?token=secret",
                    &["0".to_string()]
                )
                .expect("checked selection")
                .kind,
            RemoteFirmwareKind::DirectImageZip
        );
    }

    #[test]
    fn firmware_output_directory_capability_rejects_empty_forged_and_raw_path_ids() {
        let runtime = FirmwareOutputDirectoryRuntime::new();
        let private_directory = PathBuf::from(r"C:\private\firmware-output");
        let selection = runtime.replace(private_directory.clone());

        assert_eq!(
            runtime
                .resolve(&selection.selection_id)
                .expect("the Rust-issued capability should resolve"),
            private_directory
        );
        for forged in ["", "firmware-output-forged", r"C:\private\firmware-output"] {
            let error = runtime
                .resolve(forged)
                .expect_err("browser-provided non-capabilities must fail closed");
            assert_eq!(error, "提取输出目录选择已失效，请重新选择。");
            assert!(!error.contains("private"));
            if !forged.is_empty() {
                assert!(!error.contains(forged));
            }
        }
    }

    #[test]
    fn firmware_output_directory_selection_dto_exposes_only_the_opaque_id() {
        let runtime = FirmwareOutputDirectoryRuntime::new();
        let selection = runtime.replace(PathBuf::from(r"C:\private\firmware-output"));
        let json = serde_json::to_value(&selection).expect("selection should serialize");

        assert_eq!(json.as_object().map(serde_json::Map::len), Some(1));
        assert_eq!(json["selectionId"], selection.selection_id);
        assert!(json.get("directoryPath").is_none());
        assert!(!json.to_string().contains("private"));
        assert!(!json.to_string().contains("firmware-output\\"));
    }

    #[test]
    fn firmware_output_directory_capability_is_unpredictable_reusable_and_replaceable() {
        let runtime = FirmwareOutputDirectoryRuntime::new();
        let first_path = PathBuf::from(r"C:\private\first-output");
        let second_path = PathBuf::from(r"C:\private\second-output");
        let first = runtime.replace(first_path);
        let opaque = first
            .selection_id
            .strip_prefix("firmware-output-")
            .expect("capability should use the fixed namespace");
        let parsed = uuid::Uuid::parse_str(opaque).expect("capability should contain a UUID");

        assert_eq!(parsed.get_version(), Some(uuid::Version::Random));
        assert_eq!(
            runtime.resolve(&first.selection_id).unwrap(),
            runtime.resolve(&first.selection_id).unwrap(),
            "a current selection remains reusable for repeated remote extraction"
        );

        let second = runtime.replace(second_path.clone());
        assert_ne!(first.selection_id, second.selection_id);
        assert!(runtime.resolve(&first.selection_id).is_err());
        assert_eq!(runtime.resolve(&second.selection_id).unwrap(), second_path);

        runtime.clear();
        assert!(runtime.resolve(&second.selection_id).is_err());
    }

    #[test]
    fn remote_extraction_request_resolves_only_the_rust_issued_output_capability() {
        let runtime = FirmwareOutputDirectoryRuntime::new();
        let private_directory = PathBuf::from(r"C:\private\remote-output");
        let selection = runtime.replace(private_directory.clone());

        let request = build_remote_extraction_request(
            &runtime,
            "https://firmware.example.test/ota.zip".to_string(),
            vec!["0".to_string()],
            &selection.selection_id,
        )
        .expect("the current capability should build a request");
        assert_eq!(request.output_directory, private_directory);

        let error = match build_remote_extraction_request(
            &runtime,
            "https://firmware.example.test/ota.zip".to_string(),
            vec!["0".to_string()],
            r"C:\private\attacker-selected-output",
        ) {
            Err(error) => error,
            Ok(_) => panic!("a raw output path must not cross the remote command boundary"),
        };
        assert_eq!(error, "提取输出目录选择已失效，请重新选择。");
        assert!(!error.contains("attacker-selected-output"));
    }

    #[test]
    fn canceling_output_directory_selection_preserves_the_current_capability() {
        let runtime = FirmwareOutputDirectoryRuntime::new();
        let current_path = PathBuf::from(r"C:\private\current-output");
        let current = runtime.replace(current_path.clone());
        let picker = runtime.begin_selection();

        assert!(runtime
            .publish_selection(picker, None)
            .expect("the current picker may be canceled")
            .is_none());
        assert_eq!(
            runtime.resolve(&current.selection_id).unwrap(),
            current_path
        );
    }

    #[test]
    fn cleanup_invalidates_a_pending_picker_before_it_can_publish() {
        let runtime = FirmwareOutputDirectoryRuntime::new();
        let pending = runtime.begin_selection();
        runtime.clear();

        let error = match runtime.publish_selection(
            pending,
            Some(PathBuf::from(r"C:\private\post-logout-output")),
        ) {
            Err(error) => error,
            Ok(_) => panic!("a picker that predates cleanup must not publish"),
        };

        assert_eq!(error, "提取输出目录选择已失效，请重新选择。");
        assert!(!error.contains("post-logout-output"));
        assert!(runtime.resolve("firmware-output-forged").is_err());
    }

    #[test]
    fn newer_picker_and_its_cancelation_prevent_an_older_picker_from_overwriting_selection() {
        let runtime = FirmwareOutputDirectoryRuntime::new();
        let current_path = PathBuf::from(r"C:\private\current-output");
        let current = runtime.replace(current_path.clone());
        let older = runtime.begin_selection();
        let newer = runtime.begin_selection();

        assert!(runtime
            .publish_selection(newer, None)
            .expect("the newest picker may be canceled")
            .is_none());
        let stale = match runtime
            .publish_selection(older, Some(PathBuf::from(r"C:\private\stale-output")))
        {
            Err(error) => error,
            Ok(_) => panic!("an older picker must not overwrite the current selection"),
        };

        assert_eq!(stale, "提取输出目录选择已失效，请重新选择。");
        assert_eq!(
            runtime.resolve(&current.selection_id).unwrap(),
            current_path
        );
    }

    #[test]
    fn remote_payload_errors_do_not_expose_private_paths() {
        let error =
            remote_payload_application_error_to_domain(FirmwareExtractApplicationError::Format(
                r"payload_dumper failed at C:\Users\private\payload-output\boot.img".to_string(),
            ));
        let text = error.to_string();
        assert!(!text.contains("payload-output"));
        assert!(!text.contains("C:\\Users\\private"));
    }

    #[test]
    fn remote_zip_progress_accumulates_completed_members() {
        let mut progress = RemoteZipProgressTracker::new(10);
        assert_eq!(progress.record("boot", 4), 4);
        assert_eq!(progress.record("vendor_boot", 2), 6);
        assert_eq!(progress.record("vendor_boot", 6), 10);
    }

    #[test]
    fn inspection_dto_exposes_only_path_safe_partition_metadata() {
        let dto = firmware_inspection_dto(FirmwareExtractInspection {
            format: FirmwareFormat::VivoGzipTar,
            entries: vec![FirmwareExtractEntry {
                id: "0".to_string(),
                name: "boot.img".to_string(),
                size_bytes: 4,
            }],
        });

        assert_eq!(dto.format, "vivoGzipTar");
        assert_eq!(dto.entries[0].id, "0");
        assert_eq!(dto.entries[0].name, "boot.img");
        assert_eq!(dto.entries[0].size_bytes, 4);
    }

    #[test]
    fn extraction_dto_exposes_only_image_name_size_and_opaque_result_id() {
        let dto = firmware_extraction_dto(
            vec![nwflash_domain::FlashImageInfo {
                path: r"C:\private\firmware-stage\boot.img".to_string(),
                size_bytes: 4,
            }],
            vec![Some("result-opaque".to_string())],
        );

        assert_eq!(dto.images.len(), 1);
        assert_eq!(dto.images[0].name, "boot.img");
        assert_eq!(dto.images[0].size_bytes, 4);
        assert_eq!(dto.images[0].result_id.as_deref(), Some("result-opaque"));
    }

    #[test]
    fn payload_runtime_preserves_remote_source_only_inside_rust() {
        let runtime = PayloadInspectionRuntime::new();
        runtime.replace(
            "https://updates.example/payload.bin".to_string(),
            vec![FirmwareExtractEntry {
                id: "0".to_string(),
                name: "boot".to_string(),
                size_bytes: 4,
            }],
        );

        let selected = runtime
            .resolve_selected(&["0".to_string()])
            .expect("opaque selection should resolve");

        assert_eq!(selected.source, "https://updates.example/payload.bin");
        assert_eq!(
            selected
                .entries
                .iter()
                .map(|entry| (entry.name.as_str(), entry.size_bytes))
                .collect::<Vec<_>>(),
            vec![("boot", 4)]
        );
    }

    #[test]
    fn clearing_payload_runtime_invalidates_previous_partition_ids() {
        let runtime = PayloadInspectionRuntime::new();
        runtime.replace(
            "payload.bin".to_string(),
            vec![FirmwareExtractEntry {
                id: "0".to_string(),
                name: "boot".to_string(),
                size_bytes: 4,
            }],
        );

        runtime.clear();

        assert!(runtime.resolve_selected(&["0".to_string()]).is_err());
    }

    #[test]
    fn extraction_runtime_snapshots_managed_images_and_invalidates_old_ids() {
        let root = temp_root("extraction-runtime");
        let first_source = root.join("boot.img");
        let second_source = root.join("init_boot.img");
        fs::write(&first_source, b"boot").expect("first image should be written");
        fs::write(&second_source, b"init").expect("second image should be written");
        let runtime = FirmwareExtractionRuntime::new();
        let first_ids = runtime
            .replace(vec![nwflash_domain::FlashImageInfo {
                path: first_source.to_string_lossy().into_owned(),
                size_bytes: 4,
            }])
            .expect("first image should be snapshotted");
        let first_id = first_ids[0]
            .as_deref()
            .expect("managed image should receive an id")
            .to_string();
        let first_snapshot = runtime
            .get(&first_id)
            .expect("first snapshot should resolve")
            .image
            .path;

        let second_ids = runtime
            .replace(vec![nwflash_domain::FlashImageInfo {
                path: second_source.to_string_lossy().into_owned(),
                size_bytes: 4,
            }])
            .expect("second image should be snapshotted");

        assert!(runtime.get(&first_id).is_err());
        assert!(!Path::new(&first_snapshot).exists());
        assert!(runtime
            .get(
                second_ids[0]
                    .as_deref()
                    .expect("second image should receive an id")
            )
            .is_ok());
        runtime.clear();
        fs::remove_dir_all(root).expect("fixture directory should be removed");
    }

    #[test]
    fn replacing_an_output_file_cannot_change_the_snapshotted_flash_artifact() {
        let root = temp_root("extraction-mutation");
        let source = root.join("boot.img");
        fs::write(&source, b"boot").expect("image should be written");
        let results = FirmwareExtractionRuntime::new();
        let ids = results
            .replace(vec![nwflash_domain::FlashImageInfo {
                path: source.to_string_lossy().into_owned(),
                size_bytes: 4,
            }])
            .expect("image should be snapshotted");
        fs::write(&source, b"evil").expect("output image should be replaced");
        let artifacts = FirmwareArtifactRuntime::new();
        let staging = create_unique_firmware_root("firmware-stage")
            .expect("artifact staging root should exist");
        let prepared = stage_extracted_firmware_artifact(
            &artifacts,
            results
                .get(
                    ids[0]
                        .as_deref()
                        .expect("managed image should receive an id"),
                )
                .expect("snapshot should resolve"),
            staging,
            || false,
        )
        .expect("snapshot should become an artifact");
        let artifact = artifacts
            .get(&prepared.artifact_id)
            .expect("artifact should resolve");

        assert_eq!(
            fs::read(&artifact.image.path).expect("artifact should be readable"),
            b"boot"
        );
        results.clear();
        fs::remove_dir_all(root).expect("fixture directory should be removed");
        fs::remove_dir_all(artifact.staging_root).expect("artifact staging should be removed");
    }

    #[test]
    fn line_flash_inspection_returns_only_managed_zip_images() {
        let root = temp_root("line-package");
        let archive_path = root.join("firmware.zip");
        write_image_zip(
            &archive_path,
            &[("images/boot.img", b"boot"), ("images/super.img", b"super")],
        );

        let dto = line_flash_package_inspection(&archive_path)
            .expect("line-flash package should be inspected");

        assert_eq!(dto.format, "zip");
        assert_eq!(dto.entries.len(), 1);
        assert_eq!(dto.entries[0].name, "boot.img");
        fs::remove_dir_all(root).expect("fixture directory should be removed");
    }

    #[test]
    fn line_flash_extraction_returns_path_safe_image_metadata() {
        let root = temp_root("line-extract");
        let archive_path = root.join("firmware.zip");
        write_image_zip(&archive_path, &[("images/boot.img", b"boot")]);

        let dto = line_flash_package_extraction(&archive_path, "0", &root.join("staging"))
            .expect("managed image should be extracted");

        assert_eq!(dto.images.len(), 1);
        assert_eq!(dto.images[0].name, "boot.img");
        assert_eq!(dto.images[0].size_bytes, 4);
        assert!(dto.images[0].result_id.is_none());
        fs::remove_dir_all(root).expect("fixture directory should be removed");
    }

    #[test]
    fn firmware_artifact_runtime_uses_an_opaque_id_for_the_staged_image() {
        let runtime = FirmwareArtifactRuntime::new();
        let root = std::env::temp_dir().join("nwflash-artifact-runtime-test");
        let artifact_id = runtime.replace(
            nwflash_domain::QuickFlashPartition::Boot,
            nwflash_domain::FlashImageInfo {
                path: r"C:\private\staging\boot-unique.img".to_string(),
                size_bytes: 4,
            },
            root,
        );

        let artifact = runtime
            .get(&artifact_id)
            .expect("opaque artifact ID should resolve inside Rust");

        assert_eq!(
            artifact.partition,
            nwflash_domain::QuickFlashPartition::Boot
        );
        assert_eq!(artifact.image.size_bytes, 4);
        assert!(runtime.get(r"C:\private\staging\boot-unique.img").is_err());
    }

    #[test]
    fn firmware_progress_runtime_emits_path_safe_payloads_through_an_injected_sink() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let captured = events.clone();
        let runtime = FirmwareProgressRuntime::with_sink(move |event| {
            captured
                .lock()
                .expect("progress test sink lock should not be poisoned")
                .push(event);
        });

        let reporter = runtime.start();
        reporter.report(Some("boot.img".to_string()), 40, 80, Some(12));
        reporter.report(Some("boot.img".to_string()), 41, 80, Some(13));

        let events = events
            .lock()
            .expect("progress test event lock should not be poisoned");
        assert_eq!(events.len(), 1, "reports must remain throttled to 100 ms");
        assert_eq!(events[0].current_partition.as_deref(), Some("boot.img"));
        assert_eq!(events[0].bytes_completed, 40);
        assert_eq!(events[0].bytes_total, 80);
        assert_eq!(events[0].percentage, 50.0);
        assert_eq!(events[0].gzip_stream_bytes, Some(12));
    }

    #[test]
    fn firmware_progress_runtime_forces_terminal_measurements_through_the_sink() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let captured = events.clone();
        let runtime = FirmwareProgressRuntime::with_sink(move |event| {
            captured
                .lock()
                .expect("progress test sink lock should not be poisoned")
                .push(event);
        });

        let reporter = runtime.start();
        reporter.report(Some("boot.img".to_string()), 79, 80, Some(12));
        reporter.report_terminal(None, 80, 80, Some(16));

        let events = events
            .lock()
            .expect("progress test event lock should not be poisoned");
        assert_eq!(events.len(), 2, "terminal progress must bypass throttling");
        assert_eq!(events[1].bytes_completed, 80);
        assert_eq!(events[1].bytes_total, 80);
        assert_eq!(events[1].percentage, 100.0);
        assert_eq!(events[1].gzip_stream_bytes, Some(16));
    }

    #[test]
    fn preparing_line_flash_image_stores_a_quick_flash_artifact_by_id() {
        let root = temp_root("artifact-prepare");
        let archive_path = root.join("firmware.zip");
        write_image_zip(&archive_path, &[("images/boot.img", b"boot")]);
        let runtime = FirmwareArtifactRuntime::new();
        let artifact =
            prepare_line_flash_artifact(&runtime, &archive_path, "0", &root.join("staging"))
                .expect("managed image should be stored as an artifact");

        assert_eq!(artifact.name, "boot.img");
        assert_eq!(artifact.size_bytes, 4);
        assert!(artifact.artifact_id.starts_with("firmware-"));
        assert_eq!(
            runtime
                .get(&artifact.artifact_id)
                .expect("artifact should remain private to Rust")
                .partition,
            nwflash_domain::QuickFlashPartition::Boot
        );
        fs::remove_dir_all(root).expect("fixture directory should be removed");
    }

    #[test]
    fn canceled_line_flash_preparation_does_not_store_a_flash_artifact() {
        let root = temp_root("artifact-cancel");
        let archive_path = root.join("firmware.zip");
        write_image_zip(&archive_path, &[("images/boot.img", b"boot")]);
        let runtime = FirmwareArtifactRuntime::new();

        let error = prepare_line_flash_artifact_with_cancel(
            &runtime,
            &archive_path,
            "0",
            &root.join("staging"),
            || true,
        )
        .expect_err("canceling ZIP extraction must not prepare an artifact");

        assert!(error.to_string().contains("取消"));
        assert!(runtime
            .artifact
            .lock()
            .expect("artifact lock should be available")
            .is_none());
        fs::remove_dir_all(root).expect("fixture directory should be removed");
    }

    #[test]
    fn internal_firmware_staging_root_is_created_beneath_the_owned_temp_namespace() {
        let root = create_unique_firmware_root("firmware-stage")
            .expect("internal firmware staging root should be created");
        let expected_base = std::env::temp_dir().join("nwflash").join("firmware-stage");

        assert!(root.as_path().starts_with(&expected_base));
        assert!(root.as_path().is_dir());

        fs::remove_dir_all(root.as_path()).expect("created staging root should be removable");
    }

    #[test]
    fn replacing_owned_artifacts_cleans_only_the_superseded_internal_staging_root() {
        let root = temp_root("external-root");
        let runtime = FirmwareArtifactRuntime::new();
        runtime.replace(
            nwflash_domain::QuickFlashPartition::Boot,
            nwflash_domain::FlashImageInfo {
                path: root.join("external.img").to_string_lossy().into_owned(),
                size_bytes: 4,
            },
            root.clone(),
        );
        let first_owned = create_unique_firmware_root("firmware-stage")
            .expect("first owned staging root should be created");
        let first_owned_path = first_owned.as_path().to_path_buf();
        runtime.replace_owned(
            nwflash_domain::QuickFlashPartition::Boot,
            nwflash_domain::FlashImageInfo {
                path: first_owned_path
                    .join("boot.img")
                    .to_string_lossy()
                    .into_owned(),
                size_bytes: 4,
            },
            first_owned,
        );
        let second_owned = create_unique_firmware_root("firmware-stage")
            .expect("second owned staging root should be created");
        let second_owned_path = second_owned.as_path().to_path_buf();
        runtime.replace_owned(
            nwflash_domain::QuickFlashPartition::Boot,
            nwflash_domain::FlashImageInfo {
                path: second_owned_path
                    .join("boot.img")
                    .to_string_lossy()
                    .into_owned(),
                size_bytes: 4,
            },
            second_owned,
        );

        assert!(root.is_dir());
        assert!(!first_owned_path.exists());
        assert!(second_owned_path.is_dir());

        fs::remove_dir_all(root).expect("external fixture directory should be removed");
        fs::remove_dir_all(second_owned_path)
            .expect("second owned staging root should be removed by the test");
    }

    #[tokio::test]
    async fn line_flash_artifact_operation_stages_an_opaque_runtime_artifact() {
        let root = temp_root("artifact-operation");
        let archive_path = root.join("firmware.zip");
        write_image_zip(&archive_path, &[("images/boot.img", b"boot")]);
        let runtime = FirmwareArtifactRuntime::new();

        let prepared = prepare_line_flash_artifact_operation(
            nwflash_application::OperationCoordinator::default(),
            runtime.clone(),
            archive_path,
            "0".to_string(),
        )
        .await
        .expect("coordinated preparation should stage the managed image");

        let artifact = runtime
            .get(&prepared.artifact_id)
            .expect("prepared artifact should remain in the Rust runtime");
        assert!(prepared.artifact_id.starts_with("firmware-"));
        assert_eq!(prepared.name, "boot.img");
        assert_eq!(prepared.size_bytes, 4);
        assert!(artifact.image.path.contains("firmware-stage"));
        fs::remove_dir_all(root).expect("fixture directory should be removed");
        fs::remove_dir_all(artifact.staging_root).expect("internal staging root should be removed");
    }

    #[tokio::test]
    async fn payload_inspection_operation_uses_a_provisioned_tool_and_path_safe_metadata() {
        let root = temp_root("payload-operation");
        let executable = write_metadata_tool(&root);
        let payload_runtime = PayloadInspectionRuntime::new();

        let inspection = inspect_payload_with_provisioner_operation(
            nwflash_application::OperationCoordinator::default(),
            payload_runtime.clone(),
            test_provisioner(&root, executable),
            root.join("source.payload").to_string_lossy().into_owned(),
        )
        .await
        .expect("provisioned payload tool should inspect metadata");

        assert_eq!(inspection.format, "payload");
        assert_eq!(inspection.entries.len(), 1);
        assert_eq!(inspection.entries[0].id, "0");
        assert_eq!(inspection.entries[0].name, "boot");
        assert_eq!(inspection.entries[0].size_bytes, 4);
        assert_eq!(
            payload_runtime
                .resolve_selected(&["0".to_string()])
                .expect("inspection should replace the Rust payload snapshot")
                .entries
                .iter()
                .map(|entry| (entry.name.as_str(), entry.size_bytes))
                .collect::<Vec<_>>(),
            vec![("boot", 4)]
        );
        fs::remove_dir_all(root).expect("fixture directory should be removed");
    }

    #[tokio::test]
    async fn remote_payload_inspection_passes_the_original_url_to_payload_dumper() {
        let root = temp_root("remote-payload-inspect-url");
        let record = root.join("source.txt");
        let executable = write_recording_metadata_tool(&root, &record);
        let url = spawn_remote_payload_server(b"CrAU\x01remote-payload".to_vec());

        let inspection = inspect_remote_firmware_operation(
            nwflash_application::OperationCoordinator::default(),
            RemoteFirmwareInspectionRuntime::new(),
            test_provisioner(&root, executable),
            url.clone(),
            FirmwareProgressRuntime::new().start(),
        )
        .await
        .expect("remote payload should be inspected");

        assert_eq!(inspection.entries[0].name, "boot");
        assert_eq!(
            fs::read_to_string(&record)
                .expect("source argument should be recorded")
                .trim(),
            url
        );
        fs::remove_dir_all(root).expect("fixture directory should be removed");
    }

    #[tokio::test]
    async fn payload_extraction_operation_resolves_runtime_ids_and_hides_output_paths() {
        let root = temp_root("payload-extract-operation");
        let executable = root.join("payload_dumper.cmd");
        fs::write(
            &executable,
            "@echo off\r\nset output=\r\nset partitions=\r\n:next\r\nif \"%~1\"==\"\" goto done\r\nif \"%~1\"==\"-i\" set partitions=%~2\r\nif \"%~1\"==\"-o\" set output=%~2\r\nshift\r\ngoto next\r\n:done\r\nfor %%p in (%partitions:,= %) do >\"%output%\\%%p.img\" echo payload\r\nexit /b 0\r\n",
        )
        .expect("payload tool script should be written");
        let payload_runtime = PayloadInspectionRuntime::new();
        payload_runtime.replace(
            root.join("source.payload").to_string_lossy().into_owned(),
            vec![FirmwareExtractEntry {
                id: "0".to_string(),
                name: "boot".to_string(),
                size_bytes: 9,
            }],
        );
        let extraction_runtime = FirmwareExtractionRuntime::new();
        let output_directory = root.join("output");

        let extraction = extract_payload_with_provisioner_operation(
            nwflash_application::OperationCoordinator::default(),
            extraction_runtime.clone(),
            payload_runtime,
            test_provisioner(&root, executable),
            vec!["0".to_string()],
            output_directory.clone(),
            None,
        )
        .await
        .expect("the runtime payload selection should be extracted");

        assert_eq!(extraction.images.len(), 1);
        assert_eq!(extraction.images[0].name, "boot.img");
        assert_eq!(extraction.images[0].size_bytes, 9);
        assert!(extraction.images[0].result_id.is_some());
        assert!(output_directory.join("boot.img").is_file());
        extraction_runtime.clear();
        fs::remove_dir_all(root).expect("fixture directory should be removed");
    }

    #[tokio::test]
    async fn remote_payload_extraction_passes_the_original_url_and_selected_partition() {
        let root = temp_root("remote-payload-extract-url");
        let record = root.join("source.txt");
        let executable = write_recording_extraction_tool(&root, &record);
        let url = spawn_remote_payload_server(b"CrAU\x01remote-payload".to_vec());
        let remote_runtime = RemoteFirmwareInspectionRuntime::new();
        remote_runtime.replace(
            url.clone(),
            RemoteFirmwareKind::PayloadRaw,
            vec![FirmwareExtractEntry {
                id: "0".to_string(),
                name: "boot".to_string(),
                size_bytes: 9,
            }],
        );
        let output_directory = root.join("output");

        let extraction = extract_remote_firmware_operation(
            nwflash_application::OperationCoordinator::default(),
            FirmwareExtractionRuntime::new(),
            remote_runtime,
            test_provisioner(&root, executable),
            RemoteFirmwareExtractionRequest {
                source: url.clone(),
                selected_ids: vec!["0".to_string()],
                output_directory: output_directory.clone(),
            },
            FirmwareProgressRuntime::new().start(),
        )
        .await
        .expect("remote payload should be extracted");

        assert_eq!(extraction.images.len(), 1);
        assert_eq!(
            fs::read_to_string(&record)
                .expect("source argument should be recorded")
                .trim(),
            url
        );
        assert!(output_directory.join("boot.img").is_file());
        fs::remove_dir_all(root).expect("fixture directory should be removed");
    }

    #[tokio::test]
    async fn local_firmware_router_uses_the_provisioned_payload_path_for_crau_sources() {
        let root = temp_root("payload-router");
        let source = root.join("payload.bin");
        fs::write(&source, b"CrAU").expect("payload fixture should be written");
        let executable = write_metadata_tool(&root);

        let inspection = inspect_local_or_payload(
            nwflash_application::OperationCoordinator::default(),
            PayloadInspectionRuntime::new(),
            test_provisioner(&root, executable),
            source,
        )
        .await
        .expect("CrAU source should be inspected through payload_dumper");

        assert_eq!(inspection.format, "payload");
        assert_eq!(inspection.entries[0].name, "boot");
        fs::remove_dir_all(root).expect("fixture directory should be removed");
    }

    #[tokio::test]
    async fn local_firmware_router_uses_the_provisioned_payload_path_for_payload_zip_sources() {
        let root = temp_root("payload-zip-router");
        let source = root.join("ota.zip");
        write_image_zip(&source, &[("payload.bin", b"payload")]);
        let executable = write_metadata_tool(&root);

        let inspection = inspect_local_or_payload(
            nwflash_application::OperationCoordinator::default(),
            PayloadInspectionRuntime::new(),
            test_provisioner(&root, executable),
            source,
        )
        .await
        .expect("payload ZIP should be inspected through payload_dumper");

        assert_eq!(inspection.format, "payload");
        assert_eq!(inspection.entries[0].name, "boot");
        fs::remove_dir_all(root).expect("fixture directory should be removed");
    }

    struct CapturingUsageReporter {
        entries: Mutex<Vec<nwflash_domain::UsageLogEntry>>,
    }

    impl nwflash_application::UsageReporter for CapturingUsageReporter {
        fn record(&self, entry: nwflash_domain::UsageLogEntry) {
            self.entries
                .lock()
                .expect("usage entry capture lock should not be poisoned")
                .push(entry);
        }
    }

    fn capturing_coordinator() -> (
        nwflash_application::OperationCoordinator,
        Arc<CapturingUsageReporter>,
    ) {
        let reporter = Arc::new(CapturingUsageReporter {
            entries: Mutex::new(Vec::new()),
        });
        let coordinator = nwflash_application::OperationCoordinator::new(
            None,
            None,
            Some(reporter.clone()),
            None,
            None,
        );
        (coordinator, reporter)
    }

    #[tokio::test]
    async fn local_firmware_inspection_records_usage_details_for_non_payload_sources() {
        let root = temp_root("local-inspect-usage");
        let source = root.join("images");
        fs::create_dir_all(&source).expect("image directory should be created");
        fs::write(source.join("boot.img"), b"boot").expect("image fixture should be written");

        let (coordinator, reporter) = capturing_coordinator();
        let inspection = inspect_local_or_payload(
            coordinator,
            PayloadInspectionRuntime::new(),
            test_provisioner(&root, write_metadata_tool(&root)),
            source,
        )
        .await
        .expect("image directory source should be inspected");

        assert_eq!(inspection.format, "imageDirectory");
        let entries = reporter
            .entries
            .lock()
            .expect("usage entry capture lock should not be poisoned");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].operation, "Hashing");
        assert_eq!(entries[0].title, "检查本地固件");
        assert_eq!(entries[0].status, "success");
        assert!(entries[0]
            .details
            .iter()
            .any(|detail| detail.message.contains("本地固件检查完成")));
        fs::remove_dir_all(root).expect("fixture directory should be removed");
    }
}
