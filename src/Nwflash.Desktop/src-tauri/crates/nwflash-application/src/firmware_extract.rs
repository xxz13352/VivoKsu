use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{Read, Write},
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use nwflash_domain::FlashImageInfo;
use nwflash_infrastructure::{
    collect_required_payload_extraction_results, parse_payload_metadata, FirmwareExtractionError,
    FirmwareFormat, FirmwareFormatDetector, FirmwarePackageExtractionService,
    FirmwarePackageInspector, PayloadDumperCommand, VivoFirmwareError, VivoFirmwareExtractor,
    VivoFirmwareProgress,
};
use nwflash_windows::process::{run_command_with_cancel, ProcessCommand};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirmwareExtractEntry {
    pub id: String,
    pub name: String,
    pub size_bytes: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirmwareExtractInspection {
    pub format: FirmwareFormat,
    pub entries: Vec<FirmwareExtractEntry>,
}

#[derive(Debug, Error)]
pub enum FirmwareExtractApplicationError {
    #[error("固件格式暂不支持提取。")]
    UnsupportedFormat,
    #[error("读取 VIVO 固件失败：{0}")]
    Vivo(#[from] VivoFirmwareError),
    #[error("读取固件格式失败：{0}")]
    Format(String),
    #[error("读取固件目录失败：{0}")]
    Directory(String),
    #[error("读取 ZIP 固件包失败：{0}")]
    Zip(String),
    #[error("请选择有效且不重复的固件分区。")]
    InvalidSelection,
    #[error("固件提取已取消。")]
    Canceled,
}

pub struct FirmwareExtractService;

impl FirmwareExtractService {
    pub fn inspect_payload<F>(
        executable_path: &Path,
        payload_source: &str,
        metadata_directory: &Path,
        should_cancel: F,
    ) -> Result<FirmwareExtractInspection, FirmwareExtractApplicationError>
    where
        F: FnMut() -> bool,
    {
        fs::create_dir_all(metadata_directory)
            .map_err(|error| FirmwareExtractApplicationError::Directory(error.to_string()))?;
        let command = PayloadDumperCommand::metadata(
            executable_path.to_string_lossy(),
            payload_source,
            metadata_directory.to_string_lossy(),
        )
        .map_err(|error| FirmwareExtractApplicationError::Format(error.to_string()))?;
        let output = run_command_with_cancel(
            ProcessCommand::new(command.program, command.args),
            None,
            should_cancel,
        )
        .map_err(payload_process_error)?;
        if output.exit_code != 0 {
            return Err(FirmwareExtractApplicationError::Format(format!(
                "payload_dumper 读取元数据失败，退出码 {}。{}",
                output.exit_code, output.stderr
            )));
        }

        let metadata =
            fs::read_to_string(metadata_directory.join("metadata.json")).map_err(|error| {
                FirmwareExtractApplicationError::Format(format!(
                    "payload_dumper 未生成元数据：{error}"
                ))
            })?;
        let entries = parse_payload_metadata(&metadata)
            .map_err(|error| FirmwareExtractApplicationError::Format(error.to_string()))?
            .into_iter()
            .enumerate()
            .map(|(index, entry)| FirmwareExtractEntry {
                id: index.to_string(),
                name: entry.name,
                size_bytes: entry.size_bytes,
            })
            .collect();
        Ok(FirmwareExtractInspection {
            format: FirmwareFormat::Payload,
            entries,
        })
    }

    pub fn inspect_local(
        source_path: &Path,
    ) -> Result<FirmwareExtractInspection, FirmwareExtractApplicationError> {
        let format = FirmwareFormatDetector::detect_local(source_path)
            .map_err(|error| FirmwareExtractApplicationError::Format(error.to_string()))?;
        let entries = match format {
            FirmwareFormat::ImageDirectory => inspect_image_directory(source_path)?,
            FirmwareFormat::VivoGzipTar => VivoFirmwareExtractor::list(source_path)?
                .into_iter()
                .map(|entry| (entry.name, entry.size_bytes))
                .collect(),
            FirmwareFormat::Zip => FirmwarePackageInspector::inspect(source_path)
                .map_err(|error| FirmwareExtractApplicationError::Zip(error.to_string()))?
                .image_entries
                .into_iter()
                .filter_map(|entry| {
                    Path::new(&entry)
                        .file_name()
                        .and_then(|name| name.to_str())
                        .map(|name| (name.to_string(), 0))
                })
                .collect(),
            _ => return Err(FirmwareExtractApplicationError::UnsupportedFormat),
        }
        .into_iter()
        .enumerate()
        .map(|(index, (name, size_bytes))| FirmwareExtractEntry {
            id: index.to_string(),
            name,
            size_bytes,
        })
        .collect();
        Ok(FirmwareExtractInspection { format, entries })
    }

    pub fn inspect_line_flash_package(
        package_path: &Path,
    ) -> Result<FirmwareExtractInspection, FirmwareExtractApplicationError> {
        let entries = FirmwarePackageInspector::inspect(package_path)
            .map_err(|error| FirmwareExtractApplicationError::Zip(error.to_string()))?
            .managed_image_entries()
            .into_iter()
            .filter_map(|entry| {
                Path::new(&entry)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(|name| name.to_string())
            })
            .enumerate()
            .map(|(index, name)| FirmwareExtractEntry {
                id: index.to_string(),
                name,
                size_bytes: 0,
            })
            .collect();
        Ok(FirmwareExtractInspection {
            format: FirmwareFormat::Zip,
            entries,
        })
    }

    pub fn extract_line_flash_package(
        package_path: &Path,
        selected_id: &str,
        staging_root: &Path,
    ) -> Result<FlashImageInfo, FirmwareExtractApplicationError> {
        Self::extract_line_flash_package_with_cancel(
            package_path,
            selected_id,
            staging_root,
            || false,
        )
    }

    pub fn extract_line_flash_package_with_cancel<F>(
        package_path: &Path,
        selected_id: &str,
        staging_root: &Path,
        is_canceled: F,
    ) -> Result<FlashImageInfo, FirmwareExtractApplicationError>
    where
        F: FnMut() -> bool,
    {
        let inspection = FirmwarePackageInspector::inspect(package_path)
            .map_err(|error| FirmwareExtractApplicationError::Zip(error.to_string()))?;
        let index = selected_id
            .parse::<usize>()
            .map_err(|_| FirmwareExtractApplicationError::InvalidSelection)?;
        let entry_path = inspection
            .managed_image_entries()
            .get(index)
            .cloned()
            .ok_or(FirmwareExtractApplicationError::InvalidSelection)?;
        FirmwarePackageExtractionService::extract_with_cancel(
            &inspection,
            &entry_path,
            staging_root,
            is_canceled,
        )
        .map(|result| result.image)
        .map_err(|error| match error {
            FirmwareExtractionError::Canceled => FirmwareExtractApplicationError::Canceled,
            error => FirmwareExtractApplicationError::Zip(error.to_string()),
        })
    }

    pub fn extract_local(
        source_path: &Path,
        selected_ids: &[String],
        output_directory: &Path,
    ) -> Result<Vec<FlashImageInfo>, FirmwareExtractApplicationError> {
        Self::extract_local_with_cancel(source_path, selected_ids, output_directory, || false)
    }

    pub fn extract_local_with_cancel<F>(
        source_path: &Path,
        selected_ids: &[String],
        output_directory: &Path,
        is_canceled: F,
    ) -> Result<Vec<FlashImageInfo>, FirmwareExtractApplicationError>
    where
        F: FnMut() -> bool,
    {
        Self::extract_local_with_cancel_and_progress(
            source_path,
            selected_ids,
            output_directory,
            is_canceled,
            |_| {},
        )
    }

    pub fn extract_local_with_cancel_and_progress<F, P>(
        source_path: &Path,
        selected_ids: &[String],
        output_directory: &Path,
        mut is_canceled: F,
        report_progress: P,
    ) -> Result<Vec<FlashImageInfo>, FirmwareExtractApplicationError>
    where
        F: FnMut() -> bool,
        P: FnMut(VivoFirmwareProgress),
    {
        let format = FirmwareFormatDetector::detect_local(source_path)
            .map_err(|error| FirmwareExtractApplicationError::Format(error.to_string()))?;
        if format == FirmwareFormat::ImageDirectory {
            return export_directory_images_with_cancel(
                source_path,
                selected_ids,
                output_directory,
                &mut is_canceled,
            );
        }
        if format == FirmwareFormat::Zip {
            return export_zip_images_with_cancel(
                source_path,
                selected_ids,
                output_directory,
                &mut is_canceled,
            );
        }
        if format != FirmwareFormat::VivoGzipTar {
            return Err(FirmwareExtractApplicationError::UnsupportedFormat);
        }

        let entries = VivoFirmwareExtractor::list(source_path)?;
        let mut indexes = HashSet::new();
        let mut selected = Vec::with_capacity(selected_ids.len());
        for id in selected_ids {
            let index = id
                .parse::<usize>()
                .map_err(|_| FirmwareExtractApplicationError::InvalidSelection)?;
            if !indexes.insert(index) {
                return Err(FirmwareExtractApplicationError::InvalidSelection);
            }
            selected.push(
                entries
                    .get(index)
                    .cloned()
                    .ok_or(FirmwareExtractApplicationError::InvalidSelection)?,
            );
        }
        if selected.is_empty() {
            return Err(FirmwareExtractApplicationError::InvalidSelection);
        }

        VivoFirmwareExtractor::extract_with_cancel_and_progress(
            source_path,
            &selected,
            output_directory,
            is_canceled,
            report_progress,
        )
        .map(|results| {
            results
                .into_iter()
                .map(|result| FlashImageInfo {
                    path: result.output_path,
                    size_bytes: result.size_bytes,
                })
                .collect()
        })
        .map_err(|error| match error {
            VivoFirmwareError::Canceled => FirmwareExtractApplicationError::Canceled,
            error => FirmwareExtractApplicationError::Vivo(error),
        })
    }

    pub fn extract_payload<F>(
        executable_path: &Path,
        payload_source: &str,
        partition_names: &[String],
        output_directory: &Path,
        should_cancel: F,
    ) -> Result<Vec<FlashImageInfo>, FirmwareExtractApplicationError>
    where
        F: FnMut() -> bool,
    {
        Self::extract_payload_with_progress(
            executable_path,
            payload_source,
            partition_names,
            output_directory,
            should_cancel,
            |_, _| {},
        )
    }

    pub fn extract_payload_with_progress<F, P>(
        executable_path: &Path,
        payload_source: &str,
        partition_names: &[String],
        output_directory: &Path,
        should_cancel: F,
        report_progress: P,
    ) -> Result<Vec<FlashImageInfo>, FirmwareExtractApplicationError>
    where
        F: FnMut() -> bool,
        P: FnMut(Option<String>, u64),
    {
        Self::extract_payload_internal(
            executable_path,
            payload_source,
            partition_names,
            None,
            output_directory,
            should_cancel,
            report_progress,
        )
    }

    pub fn extract_payload_with_expected_sizes_and_progress<F, P>(
        executable_path: &Path,
        payload_source: &str,
        selected_entries: &[FirmwareExtractEntry],
        output_directory: &Path,
        should_cancel: F,
        report_progress: P,
    ) -> Result<Vec<FlashImageInfo>, FirmwareExtractApplicationError>
    where
        F: FnMut() -> bool,
        P: FnMut(Option<String>, u64),
    {
        let partition_names = selected_entries
            .iter()
            .map(|entry| entry.name.clone())
            .collect::<Vec<_>>();
        let expected_sizes = selected_entries
            .iter()
            .map(|entry| (entry.name.clone(), entry.size_bytes))
            .collect::<HashMap<_, _>>();
        Self::extract_payload_internal(
            executable_path,
            payload_source,
            &partition_names,
            Some(&expected_sizes),
            output_directory,
            should_cancel,
            report_progress,
        )
    }

    fn extract_payload_internal<F, P>(
        executable_path: &Path,
        payload_source: &str,
        partition_names: &[String],
        expected_sizes: Option<&HashMap<String, i64>>,
        output_directory: &Path,
        mut should_cancel: F,
        mut report_progress: P,
    ) -> Result<Vec<FlashImageInfo>, FirmwareExtractApplicationError>
    where
        F: FnMut() -> bool,
        P: FnMut(Option<String>, u64),
    {
        let partition_refs = partition_names
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let mut output_names = HashSet::new();
        if partition_refs
            .iter()
            .any(|name| !output_names.insert(name.to_ascii_lowercase()))
        {
            return Err(FirmwareExtractApplicationError::InvalidSelection);
        }
        let expected_total_bytes = expected_sizes
            .map(|sizes| {
                sizes
                    .values()
                    .filter_map(|size| u64::try_from(*size).ok())
                    .sum::<u64>()
            })
            .filter(|total| *total > 0);
        let staging_directory =
            std::env::temp_dir().join(format!("nwflash-payload-extract-{}", unique_suffix()));
        fs::create_dir(&staging_directory)
            .map_err(|error| FirmwareExtractApplicationError::Directory(error.to_string()))?;
        let command = PayloadDumperCommand::extract(
            executable_path.to_string_lossy(),
            payload_source,
            &partition_refs,
            staging_directory.to_string_lossy(),
        );
        let command = match command {
            Ok(command) => command,
            Err(error) => {
                let _ = fs::remove_dir_all(&staging_directory);
                return Err(FirmwareExtractApplicationError::Format(error.to_string()));
            }
        };
        if let Err(error) = fs::create_dir_all(output_directory) {
            let _ = fs::remove_dir_all(&staging_directory);
            return Err(FirmwareExtractApplicationError::Directory(
                error.to_string(),
            ));
        }
        let output = match run_command_with_cancel(
            ProcessCommand::new(command.program, command.args),
            None,
            || {
                let (current_partition, written_bytes) =
                    payload_stage_progress(&staging_directory, &partition_refs);
                let staged_progress = expected_total_bytes.map_or(written_bytes, |total| {
                    payload_progress_for_phase(written_bytes, total, 0, total / 2)
                });
                report_progress(current_partition, staged_progress);
                should_cancel()
            },
        ) {
            Ok(output) => output,
            Err(error) => {
                let _ = fs::remove_dir_all(&staging_directory);
                return Err(payload_process_error(error));
            }
        };
        if output.exit_code != 0 {
            let _ = fs::remove_dir_all(&staging_directory);
            return Err(FirmwareExtractApplicationError::Format(format!(
                "payload_dumper 执行失败，退出码 {}。{}",
                output.exit_code, output.stderr
            )));
        }

        let result =
            collect_required_payload_extraction_results(&staging_directory, &partition_refs)
                .map_err(|error| FirmwareExtractApplicationError::Format(error.to_string()))
                .and_then(|results| {
                    if let Some(expected_sizes) = expected_sizes {
                        for result in &results {
                            let expected = expected_sizes
                                .get(&result.partition_name)
                                .copied()
                                .ok_or(FirmwareExtractApplicationError::InvalidSelection)?;
                            if result.size_bytes != expected {
                                return Err(FirmwareExtractApplicationError::Format(format!(
                                    "payload_dumper 输出镜像大小与分区元数据不一致：{}。",
                                    result.partition_name
                                )));
                            }
                        }
                    }
                    publish_payload_results_with_cancel(
                        results,
                        output_directory,
                        &mut should_cancel,
                        &mut report_progress,
                        expected_total_bytes.map(|total| (total / 2, total)),
                    )
                });
        let _ = fs::remove_dir_all(&staging_directory);
        result
    }
}

fn publish_payload_results_with_cancel<F, P>(
    results: Vec<nwflash_domain::PayloadExtractionResult>,
    output_directory: &Path,
    is_canceled: &mut F,
    report_progress: &mut P,
    progress_phase: Option<(u64, u64)>,
) -> Result<Vec<FlashImageInfo>, FirmwareExtractApplicationError>
where
    F: FnMut() -> bool,
    P: FnMut(Option<String>, u64),
{
    let total_bytes = results
        .iter()
        .filter_map(|result| u64::try_from(result.size_bytes).ok())
        .sum::<u64>();
    let mut pending = Vec::with_capacity(results.len());
    let mut partial_paths = Vec::with_capacity(results.len());
    let mut promoted_paths = Vec::with_capacity(results.len());
    let mut completed_bytes = 0u64;
    let publication = (|| {
        for result in results {
            ensure_not_canceled(is_canceled)?;
            let source = File::open(&result.output_path)
                .map_err(|error| FirmwareExtractApplicationError::Directory(error.to_string()))?;
            let destination = output_directory.join(format!("{}.img", result.partition_name));
            let partial = output_directory.join(format!(
                ".{}.partial-{}",
                result.partition_name,
                unique_suffix()
            ));
            let mut source = source;
            let mut output = File::create_new(&partial)
                .map_err(|error| FirmwareExtractApplicationError::Directory(error.to_string()))?;
            partial_paths.push(partial.clone());
            let mut copied = 0u64;
            let mut buffer = [0; 8192];
            loop {
                ensure_not_canceled(is_canceled)?;
                let count = source.read(&mut buffer).map_err(|error| {
                    FirmwareExtractApplicationError::Directory(error.to_string())
                })?;
                if count == 0 {
                    break;
                }
                output.write_all(&buffer[..count]).map_err(|error| {
                    FirmwareExtractApplicationError::Directory(error.to_string())
                })?;
                copied = copied.saturating_add(count as u64);
                let published_bytes = completed_bytes.saturating_add(copied);
                let reported_bytes = progress_phase.map_or(published_bytes, |(start, end)| {
                    payload_progress_for_phase(published_bytes, total_bytes, start, end)
                });
                report_progress(Some(result.partition_name.clone()), reported_bytes);
            }
            output
                .sync_all()
                .map_err(|error| FirmwareExtractApplicationError::Directory(error.to_string()))?;
            let copied_size = i64::try_from(copied).unwrap_or(i64::MAX);
            if copied_size != result.size_bytes {
                return Err(FirmwareExtractApplicationError::Format(
                    "payload_dumper 输出镜像大小无效。".to_string(),
                ));
            }
            completed_bytes = completed_bytes.saturating_add(copied);
            pending.push((result, destination, partial));
        }

        let mut images = Vec::with_capacity(pending.len());
        for (result, destination, partial) in pending {
            ensure_not_canceled(is_canceled)?;
            fs::rename(&partial, &destination)
                .map_err(|error| FirmwareExtractApplicationError::Directory(error.to_string()))?;
            partial_paths.retain(|path| path != &partial);
            promoted_paths.push(destination.clone());
            images.push(FlashImageInfo {
                path: destination.to_string_lossy().into_owned(),
                size_bytes: result.size_bytes,
            });
        }
        Ok(images)
    })();
    if publication.is_err() {
        for partial in partial_paths {
            let _ = fs::remove_file(partial);
        }
        for promoted in promoted_paths {
            let _ = fs::remove_file(promoted);
        }
    }
    publication
}

fn payload_progress_for_phase(bytes: u64, total: u64, start: u64, end: u64) -> u64 {
    if total == 0 || end <= start {
        return end;
    }
    start.saturating_add(bytes.min(total).saturating_mul(end.saturating_sub(start)) / total)
}

fn export_directory_images_with_cancel(
    source_directory: &Path,
    selected_ids: &[String],
    output_directory: &Path,
    is_canceled: &mut impl FnMut() -> bool,
) -> Result<Vec<FlashImageInfo>, FirmwareExtractApplicationError> {
    let entries = inspect_image_directory(source_directory)?;
    let mut selected_indexes = HashSet::new();
    let mut selected = Vec::with_capacity(selected_ids.len());
    for id in selected_ids {
        let index = id
            .parse::<usize>()
            .map_err(|_| FirmwareExtractApplicationError::InvalidSelection)?;
        if !selected_indexes.insert(index) {
            return Err(FirmwareExtractApplicationError::InvalidSelection);
        }
        selected.push(
            entries
                .get(index)
                .cloned()
                .ok_or(FirmwareExtractApplicationError::InvalidSelection)?,
        );
    }
    if selected.is_empty() {
        return Err(FirmwareExtractApplicationError::InvalidSelection);
    }
    fs::create_dir_all(output_directory)
        .map_err(|error| FirmwareExtractApplicationError::Directory(error.to_string()))?;

    let mut results = Vec::with_capacity(selected.len());
    for (name, expected_size) in selected {
        ensure_not_canceled(is_canceled)?;
        let source_path = source_directory.join(&name);
        let output_path = output_directory.join(&name);
        if source_path == output_path {
            return Err(FirmwareExtractApplicationError::Directory(
                "输出目录不能与镜像来源目录相同。".to_string(),
            ));
        }
        let partial_path = output_directory.join(format!(".{name}.partial-{}", unique_suffix()));
        let result = (|| {
            let mut source = File::open(&source_path)
                .map_err(|error| FirmwareExtractApplicationError::Directory(error.to_string()))?;
            let mut partial = File::create_new(&partial_path)
                .map_err(|error| FirmwareExtractApplicationError::Directory(error.to_string()))?;
            let mut buffer = [0u8; 8192];
            loop {
                ensure_not_canceled(is_canceled)?;
                let count = source.read(&mut buffer).map_err(|error| {
                    FirmwareExtractApplicationError::Directory(error.to_string())
                })?;
                if count == 0 {
                    break;
                }
                partial.write_all(&buffer[..count]).map_err(|error| {
                    FirmwareExtractApplicationError::Directory(error.to_string())
                })?;
            }
            partial
                .sync_all()
                .map_err(|error| FirmwareExtractApplicationError::Directory(error.to_string()))?;
            let size_bytes = i64::try_from(
                fs::metadata(&partial_path)
                    .map_err(|error| FirmwareExtractApplicationError::Directory(error.to_string()))?
                    .len(),
            )
            .unwrap_or(i64::MAX);
            if size_bytes != expected_size {
                return Err(FirmwareExtractApplicationError::Directory(
                    "导出的镜像大小与来源不一致。".to_string(),
                ));
            }
            fs::rename(&partial_path, &output_path)
                .map_err(|error| FirmwareExtractApplicationError::Directory(error.to_string()))?;
            Ok(FlashImageInfo {
                path: output_path.to_string_lossy().into_owned(),
                size_bytes,
            })
        })();
        if result.is_err() {
            let _ = fs::remove_file(&partial_path);
        }
        results.push(result?);
    }
    Ok(results)
}

fn export_zip_images_with_cancel(
    source_path: &Path,
    selected_ids: &[String],
    output_directory: &Path,
    is_canceled: &mut impl FnMut() -> bool,
) -> Result<Vec<FlashImageInfo>, FirmwareExtractApplicationError> {
    let inspection = FirmwarePackageInspector::inspect(source_path)
        .map_err(|error| FirmwareExtractApplicationError::Zip(error.to_string()))?;
    let mut selected_indexes = HashSet::new();
    let mut entry_paths = Vec::with_capacity(selected_ids.len());
    for id in selected_ids {
        let index = id
            .parse::<usize>()
            .map_err(|_| FirmwareExtractApplicationError::InvalidSelection)?;
        if !selected_indexes.insert(index) {
            return Err(FirmwareExtractApplicationError::InvalidSelection);
        }
        entry_paths.push(
            inspection
                .image_entries
                .get(index)
                .cloned()
                .ok_or(FirmwareExtractApplicationError::InvalidSelection)?,
        );
    }
    if entry_paths.is_empty() {
        return Err(FirmwareExtractApplicationError::InvalidSelection);
    }

    entry_paths
        .into_iter()
        .map(|entry_path| {
            FirmwarePackageExtractionService::export_image_to_directory_with_cancel(
                &inspection,
                &entry_path,
                output_directory,
                &mut *is_canceled,
            )
            .map_err(|error| match error {
                FirmwareExtractionError::Canceled => FirmwareExtractApplicationError::Canceled,
                error => FirmwareExtractApplicationError::Zip(error.to_string()),
            })
        })
        .collect()
}

fn ensure_not_canceled(
    is_canceled: &mut impl FnMut() -> bool,
) -> Result<(), FirmwareExtractApplicationError> {
    if is_canceled() {
        return Err(FirmwareExtractApplicationError::Canceled);
    }
    Ok(())
}

fn unique_suffix() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}

fn payload_process_error(error: nwflash_domain::DomainError) -> FirmwareExtractApplicationError {
    match error {
        nwflash_domain::DomainError::UserCancelled(_) => FirmwareExtractApplicationError::Canceled,
        error => FirmwareExtractApplicationError::Format(error.to_string()),
    }
}

fn payload_stage_progress(
    staging_directory: &Path,
    partition_names: &[&str],
) -> (Option<String>, u64) {
    let mut current_partition = None;
    let mut written_bytes = 0u64;
    for partition_name in partition_names {
        let bytes = fs::metadata(staging_directory.join(format!("{partition_name}.img")))
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        if bytes > 0 {
            current_partition = Some((*partition_name).to_string());
        }
        written_bytes = written_bytes.saturating_add(bytes);
    }
    (current_partition, written_bytes)
}

fn inspect_image_directory(
    source_path: &Path,
) -> Result<Vec<(String, i64)>, FirmwareExtractApplicationError> {
    let mut images = fs::read_dir(source_path)
        .map_err(|error| FirmwareExtractApplicationError::Directory(error.to_string()))?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let metadata = entry.metadata().ok()?;
            let path = entry.path();
            let name = path.file_name()?.to_str()?.to_string();
            (metadata.is_file()
                && metadata.len() > 0
                && path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("img")))
            .then(|| (name, i64::try_from(metadata.len()).unwrap_or(i64::MAX)))
        })
        .collect::<Vec<_>>();
    images.sort_by(|left, right| {
        left.0
            .to_ascii_lowercase()
            .cmp(&right.0.to_ascii_lowercase())
            .then_with(|| left.0.cmp(&right.0))
    });
    Ok(images)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_publication_cleans_partial_files_when_copy_is_canceled() {
        let root = std::env::temp_dir().join(format!(
            "nwflash-payload-publication-cancel-{}",
            unique_suffix()
        ));
        let staging = root.join("staging");
        let output = root.join("output");
        fs::create_dir_all(&staging).expect("staging directory should be created");
        fs::create_dir_all(&output).expect("output directory should be created");
        let staged_image = staging.join("boot.img");
        fs::write(&staged_image, vec![7; 16 * 1024]).expect("staged image should be written");
        let results = vec![nwflash_domain::PayloadExtractionResult {
            partition_name: "boot".to_string(),
            output_path: staged_image.to_string_lossy().into_owned(),
            size_bytes: 16 * 1024,
        }];
        let mut cancellation_checks = 0usize;
        let mut progress = Vec::new();

        let error = publish_payload_results_with_cancel(
            results,
            &output,
            &mut || {
                cancellation_checks += 1;
                cancellation_checks > 2
            },
            &mut |partition, bytes| progress.push((partition, bytes)),
            None,
        )
        .expect_err("publication should stop between copied chunks");

        assert!(matches!(error, FirmwareExtractApplicationError::Canceled));
        assert!(progress.iter().any(|(_, bytes)| *bytes > 0));
        assert!(!output.join("boot.img").exists());
        assert!(fs::read_dir(&output)
            .expect("output directory should remain readable")
            .all(|entry| !entry
                .expect("entry should be readable")
                .file_name()
                .to_string_lossy()
                .contains("partial")));
        fs::remove_dir_all(root).expect("fixture directory should be removed");
    }

    #[test]
    fn payload_publication_rolls_back_already_promoted_images_when_canceled_during_promotion() {
        let root = std::env::temp_dir().join(format!(
            "nwflash-payload-publication-promotion-cancel-{}",
            unique_suffix()
        ));
        let staging = root.join("staging");
        let output = root.join("output");
        fs::create_dir_all(&staging).expect("staging directory should be created");
        fs::create_dir_all(&output).expect("output directory should be created");
        let boot = staging.join("boot.img");
        let vendor_boot = staging.join("vendor_boot.img");
        fs::write(&boot, [7]).expect("boot image should be written");
        fs::write(&vendor_boot, [9]).expect("vendor boot image should be written");
        let results = vec![
            nwflash_domain::PayloadExtractionResult {
                partition_name: "boot".to_string(),
                output_path: boot.to_string_lossy().into_owned(),
                size_bytes: 1,
            },
            nwflash_domain::PayloadExtractionResult {
                partition_name: "vendor_boot".to_string(),
                output_path: vendor_boot.to_string_lossy().into_owned(),
                size_bytes: 1,
            },
        ];
        let mut cancellation_checks = 0usize;

        let error = publish_payload_results_with_cancel(
            results,
            &output,
            &mut || {
                cancellation_checks += 1;
                cancellation_checks >= 8
            },
            &mut |_, _| {},
            None,
        )
        .expect_err("publication should stop before promoting the second image");

        assert!(matches!(error, FirmwareExtractApplicationError::Canceled));
        assert!(!output.join("boot.img").exists());
        assert!(!output.join("vendor_boot.img").exists());
        assert!(fs::read_dir(&output)
            .expect("output directory should remain readable")
            .all(|entry| !entry
                .expect("entry should be readable")
                .file_name()
                .to_string_lossy()
                .contains("partial")));
        fs::remove_dir_all(root).expect("fixture directory should be removed");
    }

    #[test]
    fn payload_progress_phases_are_monotonic_and_finish_at_the_metadata_total() {
        assert_eq!(payload_progress_for_phase(100, 100, 0, 50), 50);
        assert_eq!(payload_progress_for_phase(0, 100, 50, 100), 50);
        assert_eq!(payload_progress_for_phase(100, 100, 50, 100), 100);
    }
}
