use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use nwflash_domain::FlashImageInfo;
use nwflash_infrastructure::payload::remote::locate_payload_in_zip;
use nwflash_infrastructure::payload::Payload;
use nwflash_infrastructure::{
    FirmwareExtractionError, FirmwareFormat, FirmwareFormatDetector,
    FirmwarePackageExtractionService, FirmwarePackageInspector, VivoFirmwareError,
    VivoFirmwareExtractor, VivoFirmwareProgress,
};
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
    /// 读取 payload 的分区清单。
    ///
    /// 内建实现取代了原先的 `payload_dumper --metadata` 子进程调用：不再有
    /// 外部 exe、不再依赖它写出的 `metadata.json`，也不再需要「无进展判死」。
    ///
    /// `metadata_directory` 保留在签名里只为兼容既有调用方；内建实现不写盘。
    pub fn inspect_payload<F>(
        _executable_path: &Path,
        payload_source: &str,
        _metadata_directory: &Path,
        should_cancel: F,
    ) -> Result<FirmwareExtractInspection, FirmwareExtractApplicationError>
    where
        F: FnMut() -> bool,
    {
        let mut should_cancel = should_cancel;
        let payload = open_payload(payload_source, &mut should_cancel)?;
        let entries = payload
            .inner
            .manifest
            .partitions
            .iter()
            .enumerate()
            .map(|(index, partition)| FirmwareExtractEntry {
                id: index.to_string(),
                name: partition.name.clone(),
                size_bytes: i64::try_from(partition.new_size).unwrap_or(i64::MAX),
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
            // 镜像目录与 ZIP 只向 UI 暴露受控分区镜像（boot / init_boot /
            // vendor_boot / lk）。Vivo gzip tar 保持全量列表：它的条目来自
            // 设备分区表本身，由用户自行选择，与 C# 参考一致。
            FirmwareFormat::ImageDirectory => inspect_image_directory(source_path)?
                .into_iter()
                .filter(|(name, _)| is_managed_image_name(name))
                .collect::<Vec<(String, i64)>>(),
            FirmwareFormat::VivoGzipTar => VivoFirmwareExtractor::list(source_path)?
                .into_iter()
                .map(|entry| (entry.name, entry.size_bytes))
                .collect(),
            FirmwareFormat::Zip => FirmwarePackageInspector::inspect(source_path)
                .map_err(|error| FirmwareExtractApplicationError::Zip(error.to_string()))?
                .managed_image_entries()
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
        _executable_path: &Path,
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

        // 先解析 manifest：分区名与尺寸都以它为准，不再依赖调用方传入的
        // expected_sizes（后者仅用于交叉校验）。
        let mut opened = open_payload(payload_source, &mut should_cancel)?;
        if should_cancel() {
            return Err(FirmwareExtractApplicationError::Canceled);
        }

        // 分区名必须在 manifest 里存在，否则早失败——不要等写到一半才发现。
        for name in &partition_refs {
            if !opened
                .inner
                .manifest
                .partitions
                .iter()
                .any(|partition| partition.name == *name)
            {
                return Err(FirmwareExtractApplicationError::Format(format!(
                    "固件中不存在分区 {name}。"
                )));
            }
        }

        let total_bytes: u64 = partition_refs
            .iter()
            .filter_map(|name| {
                opened
                    .inner
                    .manifest
                    .partitions
                    .iter()
                    .find(|partition| partition.name == *name)
                    .map(|partition| partition.new_size)
            })
            .sum();

        // 交叉校验：调用方给的尺寸（来自先前的 inspect）必须与 manifest 一致。
        // 不一致说明两次读到的固件不同（例如 URL 背后换了内容），宁可失败。
        if let Some(expected_sizes) = expected_sizes {
            for name in &partition_refs {
                if let Some(expected) = expected_sizes.get(*name) {
                    let declared = opened
                        .inner
                        .manifest
                        .partitions
                        .iter()
                        .find(|partition| partition.name == *name)
                        .map(|partition| partition.new_size)
                        .unwrap_or_default();
                    if u64::try_from(*expected).ok() != Some(declared) {
                        return Err(FirmwareExtractApplicationError::Format(format!(
                            "分区 {name} 的尺寸与固件清单不一致，请重新读取固件。"
                        )));
                    }
                }
            }
        }

        fs::create_dir_all(output_directory)
            .map_err(|error| FirmwareExtractApplicationError::Directory(error.to_string()))?;

        // 两个阶段：先解压到暂存目录，再拷贝发布。进度必须**全程单调不减**，
        // 否则 UI 的进度条会倒退。所以解压阶段映射到前一半，发布阶段映射到后一半。
        let extraction_span = total_bytes / 2;
        let publish_span = total_bytes.saturating_sub(extraction_span);

        // 提取到私有暂存目录，全部成功后再发布到目标目录——保留「要么全有、
        // 要么全无」语义：中途取消或失败不会在用户目录里留下半截镜像。
        let staging_directory =
            std::env::temp_dir().join(format!("nwflash-payload-extract-{}", unique_suffix()));
        fs::create_dir(&staging_directory)
            .map_err(|error| FirmwareExtractApplicationError::Directory(error.to_string()))?;

        // 进度必须跨分区累加：解析器的回调只报「当前分区已写量」，而 UI 要的是
        // 总体进度。累加器负责把每分区的量结算进总量。
        let mut accumulator = PartitionProgress::default();
        let results = {
            let mut on_progress = |name: &str, written: u64, partition_total: u64| {
                let overall = accumulator.advance(name, written, partition_total);
                report_progress(
                    Some(name.to_string()),
                    scale_progress(overall, total_bytes, 0, extraction_span),
                );
            };
            match opened.inner.extract_partitions_with_cancel(
                &partition_refs,
                &staging_directory,
                |name, written, total| on_progress(name, written, total),
                || should_cancel(),
            ) {
                Ok(results) => results,
                Err(error) => {
                    let _ = fs::remove_dir_all(&staging_directory);
                    return Err(payload_error_to_application(error));
                }
            }
        };

        let published = publish_extracted_partitions(
            &results,
            &staging_directory,
            output_directory,
            &mut should_cancel,
            &mut report_progress,
            extraction_span,
            publish_span,
        );
        let _ = fs::remove_dir_all(&staging_directory);
        published
    }
}

/// 把 `[0, total]` 区间的进度线性映射到 `[base, base + span]`。
///
/// 用来让多阶段流程（解压 → 发布）的进度合起来仍是单调递增的一条线。
fn scale_progress(value: u64, total: u64, base: u64, span: u64) -> u64 {
    if total == 0 {
        return base;
    }
    let scaled = (value.min(total) as u128 * span as u128) / total as u128;
    base.saturating_add(scaled as u64)
}

/// 把「当前分区的已写量」换算成「全部所选分区的已写总量」。
///
/// 解析器逐分区回调，进入新分区时把上一个分区的总量结算进来。
#[derive(Default)]
struct PartitionProgress {
    current_partition: String,
    completed_bytes: u64,
    current_total: u64,
}

impl PartitionProgress {
    fn advance(&mut self, partition: &str, written: u64, partition_total: u64) -> u64 {
        if partition != self.current_partition {
            self.completed_bytes = self.completed_bytes.saturating_add(self.current_total);
            self.current_partition = partition.to_string();
            self.current_total = partition_total;
        }
        self.completed_bytes.saturating_add(written)
    }
}

/// 把内建解析器的错误映射到应用层错误。
fn payload_error_to_application(
    error: nwflash_infrastructure::payload::PayloadError,
) -> FirmwareExtractApplicationError {
    use nwflash_infrastructure::payload::PayloadError as Source;
    match error {
        Source::MissingPartition(name) => {
            FirmwareExtractApplicationError::Format(format!("固件中不存在分区 {name}。"))
        }
        Source::Canceled => FirmwareExtractApplicationError::Canceled,
        Source::DifferentialUnsupported => FirmwareExtractApplicationError::Format(
            "该固件是差分包，需要基础版本镜像才能提取。".to_string(),
        ),
        Source::MissingMagic | Source::UnsupportedVersion(_) | Source::Corrupt(_) => {
            FirmwareExtractApplicationError::Format("固件 payload 格式无效或已损坏。".to_string())
        }
        Source::UnsupportedOperation(op) => FirmwareExtractApplicationError::Format(format!(
            "固件使用了暂不支持的压缩方式（operation 类型 {op}）。"
        )),
        Source::Io(error) => FirmwareExtractApplicationError::Directory(error.to_string()),
    }
}

/// 把暂存目录里已提取的镜像发布到用户输出目录。
///
/// 每个镜像先写成 `.partial-` 再 rename，保证用户看到的永远是完整文件；
/// 失败或取消时清理已写出的部分文件，不留残骸。
fn publish_extracted_partitions<F, P>(
    results: &[nwflash_infrastructure::payload::ExtractedPartition],
    staging_directory: &Path,
    output_directory: &Path,
    is_canceled: &mut F,
    report_progress: &mut P,
    base_progress: u64,
    span_progress: u64,
) -> Result<Vec<FlashImageInfo>, FirmwareExtractApplicationError>
where
    F: FnMut() -> bool,
    P: FnMut(Option<String>, u64),
{
    let total_bytes: u64 = results.iter().map(|result| result.total_bytes).sum();
    let mut pending = Vec::with_capacity(results.len());
    let mut partial_paths = Vec::with_capacity(results.len());
    let mut promoted: Vec<PathBuf> = Vec::with_capacity(results.len());
    // rename 时被顶掉的旧文件的备份：`(备份路径, 原目标路径)`。
    // 失败回滚时原样放回，成功发布后才清除。
    let mut backups: Vec<(PathBuf, PathBuf)> = Vec::with_capacity(results.len());
    let mut backups_to_clean: Vec<PathBuf> = Vec::with_capacity(results.len());
    let mut completed_bytes = 0u64;
    let publication = (|| {
        for result in results {
            ensure_not_canceled(is_canceled)?;
            let source_path = staging_directory.join(format!("{}.img", result.name));
            let mut source = File::open(&source_path)
                .map_err(|error| FirmwareExtractApplicationError::Directory(error.to_string()))?;
            let destination = output_directory.join(format!("{}.img", result.name));
            let partial =
                output_directory.join(format!(".{}.partial-{}", result.name, unique_suffix()));
            let mut output = File::create_new(&partial)
                .map_err(|error| FirmwareExtractApplicationError::Directory(error.to_string()))?;
            partial_paths.push(partial.clone());

            let mut copied = 0u64;
            let mut buffer = [0u8; 8192];
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
                report_progress(
                    Some(result.name.clone()),
                    scale_progress(
                        completed_bytes.saturating_add(copied),
                        total_bytes,
                        base_progress,
                        span_progress,
                    ),
                );
            }
            output
                .sync_all()
                .map_err(|error| FirmwareExtractApplicationError::Directory(error.to_string()))?;
            drop(output);

            completed_bytes = completed_bytes.saturating_add(copied);
            pending.push((
                partial.clone(),
                destination.clone(),
                result.name.clone(),
                copied,
            ));
            report_progress(
                Some(result.name.clone()),
                scale_progress(completed_bytes, total_bytes, base_progress, span_progress),
            );
        }

        // 全部写完才 rename，保证「要么全有、要么全无」。
        //
        // 已 rename 的文件要记账：取消或失败发生在 rename 中途时，必须把这些
        // 已经露在用户目录里的文件删掉。否则用户会拿到「一半新、一半没动」的
        // 镜像集合，比彻底失败更危险（刷机时可能新旧镜像混用）。
        //
        // 旧文件不能直接删：`fs::rename` 在 Windows 上不允许覆盖已存在目标，
        // 所以先把旧文件挪成备份，失败时**原样放回**——「失败 = 什么都没变」
        // 对用户已有的镜像同样成立，删除顶掉的旧文件是不可逆的破坏。
        for (partial, destination, _, _) in &pending {
            ensure_not_canceled(is_canceled)?;
            let mut backup: Option<PathBuf> = None;
            if destination.exists() {
                let backup_path = output_directory.join(format!(
                    ".{}.previous-{}",
                    destination
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    unique_suffix()
                ));
                fs::rename(destination, &backup_path).map_err(|error| {
                    FirmwareExtractApplicationError::Directory(format!(
                        "备份已存在的镜像失败：{error}"
                    ))
                })?;
                backup = Some(backup_path);
            }
            match fs::rename(partial, destination) {
                Ok(()) => {
                    if let Some(backup_path) = backup {
                        backups.push((backup_path, destination.clone()));
                    }
                    promoted.push(destination.clone());
                }
                Err(error) => {
                    if let Some(backup_path) = backup {
                        let _ = fs::rename(&backup_path, destination);
                    }
                    return Err(FirmwareExtractApplicationError::Directory(
                        error.to_string(),
                    ));
                }
            }
        }
        // 发布成功后备份才真正清除；在此之前它们是失败回滚的依据。
        backups_to_clean = backups.iter().map(|(backup, _)| backup.clone()).collect();
        Ok(())
    })();

    if let Err(error) = publication {
        for path in &partial_paths {
            let _ = fs::remove_file(path);
        }
        // 回滚已经发布出去的文件，让「失败 = 什么都没变」成立。
        for path in &promoted {
            let _ = fs::remove_file(path);
        }
        // 被顶掉的旧文件原样放回。
        for (backup, destination) in &backups {
            let _ = fs::rename(backup, destination);
        }
        return Err(error);
    }

    for path in &backups_to_clean {
        let _ = fs::remove_file(path);
    }

    Ok(pending
        .into_iter()
        .map(|(_, destination, _name, size)| FlashImageInfo {
            path: destination.to_string_lossy().into_owned(),
            size_bytes: i64::try_from(size).unwrap_or(i64::MAX),
        })
        .collect())
}

fn export_directory_images_with_cancel(
    source_directory: &Path,
    selected_ids: &[String],
    output_directory: &Path,
    is_canceled: &mut impl FnMut() -> bool,
) -> Result<Vec<FlashImageInfo>, FirmwareExtractApplicationError> {
    let entries = inspect_image_directory(source_directory)?;
    let entries: Vec<_> = entries
        .into_iter()
        .filter(|(name, _)| is_managed_image_name(name))
        .collect();
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
        // 索引按受控白名单列表编号，拒绝从全量列表换算来的越界/错位选择。
        let entry_path = inspection
            .managed_image_entries()
            .get(index)
            .cloned()
            .ok_or(FirmwareExtractApplicationError::InvalidSelection)?;
        entry_paths.push(entry_path);
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

/// 受控分区镜像名：与 `FirmwarePackageInspection::managed_image_entries`
/// 及 C# 参考的 `ManagedPartitionNames` 一致。
fn is_managed_image_name(name: &str) -> bool {
    let stem = Path::new(name)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default();
    matches!(
        stem.to_ascii_lowercase().as_str(),
        "boot" | "init_boot" | "vendor_boot" | "lk"
    )
}

fn unique_suffix() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}

/// 已打开并解析好的 payload。
///
/// 源统一装箱成 `Read + Seek`：本地文件与远程 Range 读取器的差别只在
/// 「`Seek` 是改文件指针还是发 HTTP 请求」，解析器不该知道这个区别。
pub struct OpenedPayload {
    inner: Payload<Box<dyn ReadSeek>>,
}

/// 解析器需要的全部能力。
pub trait ReadSeek: Read + Seek {}
impl<T: Read + Seek> ReadSeek for T {}

/// 打开 payload 源（本地路径或 HTTP URL）并解析其清单。
///
/// 取代原先「调外部 `payload_dumper --metadata` 再读 `metadata.json`」的流程：
/// 现在直接在进程内解析，进度与错误都不再经过子进程边界。
fn open_payload<F>(
    payload_source: &str,
    should_cancel: &mut F,
) -> Result<OpenedPayload, FirmwareExtractApplicationError>
where
    F: FnMut() -> bool,
{
    if should_cancel() {
        return Err(FirmwareExtractApplicationError::Canceled);
    }
    if is_remote_source(payload_source) {
        return open_remote_payload(payload_source);
    }
    let path = PathBuf::from(payload_source);
    let mut file = File::open(&path).map_err(|error| {
        FirmwareExtractApplicationError::Format(format!("打开固件失败：{error}"))
    })?;
    let total = file
        .metadata()
        .map_err(|error| FirmwareExtractApplicationError::Format(error.to_string()))?
        .len();

    // 固件可能是裸 payload.bin，也可能是包着 payload.bin 的 OTA zip。
    // 后者要先在 zip 里定位成员——和远程路径共用同一套定位逻辑。
    let mut magic = [0u8; 4];
    if file.read_exact(&mut magic).is_ok() && &magic == b"PK\x03\x04" {
        let location = locate_payload_in_zip_in_file(&mut file, total)?;
        let payload = Payload::from_reader_at(Box::new(file) as Box<dyn ReadSeek>, total, location)
            .map_err(|error| FirmwareExtractApplicationError::Format(error.to_string()))?;
        return Ok(OpenedPayload { inner: payload });
    }

    let payload = Payload::from_reader(Box::new(file) as Box<dyn ReadSeek>, total)
        .map_err(|error| FirmwareExtractApplicationError::Format(error.to_string()))?;
    Ok(OpenedPayload { inner: payload })
}

/// 在本地 zip 里定位 `payload.bin` 的绝对数据偏移。
///
/// 只需读尾部窗口（中央目录）加本地头那 30 字节，不必把整个固件读进内存——
/// 真实 OTA 是 GB 级的。
fn locate_payload_in_zip_in_file(
    file: &mut File,
    total: u64,
) -> Result<u64, FirmwareExtractApplicationError> {
    let window = 4u64 * 1024 * 1024;
    let base = total.saturating_sub(window);
    let size = (total - base) as usize;
    let mut tail = vec![0u8; size];
    file.seek(SeekFrom::Start(base))
        .and_then(|_| file.read_exact(&mut tail))
        .map_err(|error| {
            FirmwareExtractApplicationError::Format(format!("读取压缩包尾部失败：{error}"))
        })?;

    let location = locate_payload_in_zip(&tail, base).map_err(|error| {
        FirmwareExtractApplicationError::Format(format!("压缩包中未找到 payload.bin：{error}"))
    })?;

    let mut local_header = [0u8; 30];
    file.seek(SeekFrom::Start(location.local_header_offset))
        .and_then(|_| file.read_exact(&mut local_header))
        .map_err(|error| {
            FirmwareExtractApplicationError::Format(format!("读取 payload.bin 本地头失败：{error}"))
        })?;
    location.data_offset(&local_header).map_err(|error| {
        FirmwareExtractApplicationError::Format(format!("定位 payload.bin 失败：{error}"))
    })
}

fn is_remote_source(source: &str) -> bool {
    let source = source.trim();
    source.starts_with("https://") || source.starts_with("http://")
}

fn open_remote_payload(
    payload_source: &str,
) -> Result<OpenedPayload, FirmwareExtractApplicationError> {
    // 远程固件可能是裸 payload.bin，也可能是包着 payload.bin 的 zip。
    // 定位逻辑复用基础设施层的实现——它内部走项目既有的 Range HTTP 读取器，
    // 已经处理过 Vivo 固件 CDN 的长 Range 断流重试。
    let url = payload_source.trim().to_string();
    let (span, reader) = nwflash_infrastructure::payload::remote::locate_remote_payload(&url)
        .map_err(|error| {
            FirmwareExtractApplicationError::Format(format!("REMOTE_LOCATE_FAILED: {error}"))
        })?;

    let payload = Payload::from_reader_at(
        Box::new(reader) as Box<dyn ReadSeek>,
        span.total_len,
        span.data_offset,
    )
    .map_err(|error| FirmwareExtractApplicationError::Format(error.to_string()))?;
    Ok(OpenedPayload { inner: payload })
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
    use nwflash_infrastructure::payload::ExtractedPartition;

    fn staged(name: &str, bytes: u64) -> ExtractedPartition {
        ExtractedPartition {
            name: name.to_string(),
            output_path: PathBuf::from(format!("{name}.img")),
            bytes_written: bytes,
            total_bytes: bytes,
        }
    }

    /// 取消发生在拷贝途中时，不能留下任何半截文件——用户目录里要么有完整镜像，
    /// 要么什么都没有。
    #[test]
    fn publication_cleans_partial_files_when_copy_is_canceled() {
        let root = std::env::temp_dir().join(format!(
            "nwflash-payload-publication-cancel-{}",
            unique_suffix()
        ));
        let staging = root.join("staging");
        let output = root.join("output");
        fs::create_dir_all(&staging).expect("staging 目录");
        fs::create_dir_all(&output).expect("output 目录");
        fs::write(staging.join("boot.img"), vec![7u8; 16 * 1024]).expect("写入暂存镜像");

        let results = vec![staged("boot", 16 * 1024)];
        let mut checks = 0usize;
        let mut progress = Vec::new();

        let error = publish_extracted_partitions(
            &results,
            &staging,
            &output,
            &mut || {
                checks += 1;
                checks > 2
            },
            &mut |partition, bytes| progress.push((partition, bytes)),
            0,
            16 * 1024,
        )
        .expect_err("拷贝途中取消应当报错");

        assert!(matches!(error, FirmwareExtractApplicationError::Canceled));
        assert!(
            progress.iter().any(|(_, bytes)| *bytes > 0),
            "取消前应有进度"
        );
        assert!(!output.join("boot.img").exists(), "不得留下目标文件");
        assert!(
            fs::read_dir(&output)
                .expect("output 可读")
                .all(|entry| !entry
                    .expect("entry 可读")
                    .file_name()
                    .to_string_lossy()
                    .contains("partial")),
            "不得留下 .partial- 残骸"
        );
        fs::remove_dir_all(root).expect("清理");
    }

    /// 多个分区时，任何一个失败都不能让前面的分区「偷偷发布」成功——
    /// 否则用户会拿到一套残缺的镜像集合，且难以察觉。
    #[test]
    fn publication_publishes_nothing_when_canceled_during_promotion() {
        let root = std::env::temp_dir().join(format!(
            "nwflash-payload-promotion-cancel-{}",
            unique_suffix()
        ));
        let staging = root.join("staging");
        let output = root.join("output");
        fs::create_dir_all(&staging).expect("staging 目录");
        fs::create_dir_all(&output).expect("output 目录");
        fs::write(staging.join("boot.img"), [7u8]).expect("写入 boot");
        fs::write(staging.join("vendor_boot.img"), [9u8]).expect("写入 vendor_boot");

        let results = vec![staged("boot", 1), staged("vendor_boot", 1)];
        let mut checks = 0usize;

        let error = publish_extracted_partitions(
            &results,
            &staging,
            &output,
            &mut || {
                checks += 1;
                // 在两个分区都拷完之后、开始 rename 之前取消。
                checks >= 8
            },
            &mut |_, _| {},
            0,
            2,
        )
        .expect_err("发布阶段取消应当报错");

        assert!(matches!(error, FirmwareExtractApplicationError::Canceled));
        assert!(!output.join("boot.img").exists(), "第一个分区也不得发布");
        assert!(!output.join("vendor_boot.img").exists());
        assert!(
            fs::read_dir(&output)
                .expect("output 可读")
                .all(|entry| !entry
                    .expect("entry 可读")
                    .file_name()
                    .to_string_lossy()
                    .contains("partial")),
            "不得留下 .partial- 残骸"
        );
        fs::remove_dir_all(root).expect("清理");
    }

    /// 用户目录里**已有**同名镜像时，发布中途取消必须把旧镜像原样放回——
    /// 删除顶掉的旧文件是不可逆的破坏，「失败 = 什么都没变」对旧文件同样成立。
    #[test]
    fn publication_restores_previous_images_when_promotion_is_canceled() {
        let root = std::env::temp_dir().join(format!(
            "nwflash-payload-restore-cancel-{}",
            unique_suffix()
        ));
        let staging = root.join("staging");
        let output = root.join("output");
        fs::create_dir_all(&staging).expect("staging 目录");
        fs::create_dir_all(&output).expect("output 目录");
        fs::write(staging.join("boot.img"), [7u8]).expect("写入暂存 boot");
        fs::write(staging.join("vendor_boot.img"), [9u8]).expect("写入暂存 vendor_boot");
        // 用户已有的旧镜像（只存在于 output，不在 staging）。
        fs::write(output.join("boot.img"), b"old-boot").expect("写入旧 boot");

        let results = vec![staged("boot", 1), staged("vendor_boot", 1)];
        let mut checks = 0usize;

        let error = publish_extracted_partitions(
            &results,
            &staging,
            &output,
            &mut || {
                checks += 1;
                // 两次拷贝（各 2 次检查）之后，第 1 次发布迭代已完成、
                // 第 2 次迭代开始时取消——boot 已被顶掉，vendor_boot 未动。
                checks >= 6
            },
            &mut |_, _| {},
            0,
            2,
        )
        .expect_err("发布阶段取消应当报错");

        assert!(matches!(error, FirmwareExtractApplicationError::Canceled));
        assert_eq!(
            fs::read(output.join("boot.img")).expect("旧 boot 必须被原样放回"),
            b"old-boot",
            "发布中途取消不得丢失用户已有的镜像"
        );
        assert!(!output.join("vendor_boot.img").exists());
        assert!(
            fs::read_dir(&output).expect("output 可读").all(|entry| {
                let name = entry.expect("entry 可读").file_name();
                !name.to_string_lossy().contains("partial")
                    && !name.to_string_lossy().contains("previous")
            }),
            "不得留下 .partial- / .previous- 残骸"
        );
        fs::remove_dir_all(root).expect("清理");
    }

    /// 全部成功时应当发布完整镜像，且进度终值等于总字节数。
    #[test]
    fn publication_promotes_every_image_on_success() {
        let root = std::env::temp_dir().join(format!(
            "nwflash-payload-publication-ok-{}",
            unique_suffix()
        ));
        let staging = root.join("staging");
        let output = root.join("output");
        fs::create_dir_all(&staging).expect("staging 目录");
        fs::create_dir_all(&output).expect("output 目录");
        fs::write(staging.join("boot.img"), [1u8, 2, 3, 4]).expect("写入 boot");
        fs::write(staging.join("vendor_boot.img"), [5u8, 6]).expect("写入 vendor_boot");

        let results = vec![staged("boot", 4), staged("vendor_boot", 2)];
        let mut progress: Vec<u64> = Vec::new();

        let images = publish_extracted_partitions(
            &results,
            &staging,
            &output,
            &mut || false,
            &mut |_, bytes| progress.push(bytes),
            0,
            6,
        )
        .expect("发布应当成功");

        assert_eq!(images.len(), 2);
        assert_eq!(
            fs::read(output.join("boot.img")).expect("读取 boot"),
            vec![1u8, 2, 3, 4]
        );
        assert_eq!(
            fs::read(output.join("vendor_boot.img")).expect("读取 vendor_boot"),
            vec![5u8, 6]
        );
        assert_eq!(progress.last().copied(), Some(6), "进度终值等于总字节数");
        assert!(
            progress.windows(2).all(|w| w[0] <= w[1]),
            "进度必须单调不减: {progress:?}"
        );
        assert!(
            fs::read_dir(&output).expect("output 可读").all(|entry| {
                let name = entry.expect("entry 可读").file_name();
                !name.to_string_lossy().contains("previous")
            }),
            "成功发布后不得留下 .previous- 备份"
        );
        fs::remove_dir_all(root).expect("清理");
    }

    /// 已有同名镜像时成功发布：新内容生效，旧内容作为备份被清理。
    #[test]
    fn publication_overwrites_previous_images_on_success() {
        let root =
            std::env::temp_dir().join(format!("nwflash-payload-overwrite-ok-{}", unique_suffix()));
        let staging = root.join("staging");
        let output = root.join("output");
        fs::create_dir_all(&staging).expect("staging 目录");
        fs::create_dir_all(&output).expect("output 目录");
        fs::write(staging.join("boot.img"), b"new-boot").expect("写入暂存 boot");
        fs::write(output.join("boot.img"), b"old-boot").expect("写入旧 boot");

        let results = vec![staged("boot", 8)];
        let images = publish_extracted_partitions(
            &results,
            &staging,
            &output,
            &mut || false,
            &mut |_, _| {},
            0,
            8,
        )
        .expect("发布应当成功");

        assert_eq!(images.len(), 1);
        assert_eq!(
            fs::read(output.join("boot.img")).expect("读取 boot"),
            b"new-boot",
            "成功发布必须用新镜像覆盖旧镜像"
        );
        fs::remove_dir_all(root).expect("清理");
    }

    /// 进度累加器：解析器逐分区回调「当前分区已写量」，UI 要的是总体进度。
    #[test]
    fn partition_progress_accumulates_across_partitions() {
        let mut accumulator = PartitionProgress::default();
        // 分区 A 总量 100，写到 40。
        assert_eq!(accumulator.advance("a", 40, 100), 40);
        // 切到分区 B（总量 50），A 的 100 应已结算进来。
        assert_eq!(accumulator.advance("b", 10, 50), 110);
        assert_eq!(accumulator.advance("b", 50, 50), 150);
    }

    /// 同一分区内不得重复结算。
    #[test]
    fn partition_progress_does_not_double_count_within_a_partition() {
        let mut accumulator = PartitionProgress::default();
        assert_eq!(accumulator.advance("a", 10, 100), 10);
        assert_eq!(accumulator.advance("a", 20, 100), 20);
        assert_eq!(accumulator.advance("a", 100, 100), 100);
    }
}
