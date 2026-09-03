use std::{
    collections::HashSet,
    fs::{self, File},
    io::{Read, Write},
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use flate2::read::GzDecoder;
use nwflash_domain::PayloadExtractionResult;
use thiserror::Error;

const STREAM_BUFFER_SIZE: usize = 8192;

struct CountingReader<R> {
    inner: R,
    bytes_read: Arc<AtomicU64>,
}

impl<R: Read> Read for CountingReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let count = self.inner.read(buffer)?;
        self.bytes_read.fetch_add(count as u64, Ordering::Relaxed);
        Ok(count)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VivoFirmwareEntry {
    pub name: String,
    pub full_path: String,
    pub size_bytes: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VivoFirmwareProgress {
    pub current_entry: String,
    pub gzip_stream_bytes: u64,
    pub entry_bytes: u64,
    pub completed_bytes: u64,
    pub total_bytes: u64,
}

#[derive(Debug, Error)]
pub enum VivoFirmwareError {
    #[error("Vivo 固件包读取失败：{0}")]
    Read(String),
    #[error("Vivo 固件 tar 条目不完整：{0}")]
    Truncated(String),
    #[error("Vivo 固件提取已取消。")]
    Canceled,
}

pub struct VivoFirmwareExtractor;

impl VivoFirmwareExtractor {
    pub fn list(archive_path: &Path) -> Result<Vec<VivoFirmwareEntry>, VivoFirmwareError> {
        let (mut tar, _) = open_tar(archive_path)?;
        let mut entries = Vec::new();
        let mut pending_long_path = None;
        while let Some(header) = read_header(&mut tar)? {
            let (header_path, size, type_flag) = parse_header(&header)?;
            if type_flag == b'L' {
                pending_long_path = Some(read_entry_text(&mut tar, size)?);
                skip_exact(&mut tar, padded_size(size)? - size)?;
                continue;
            }
            if matches!(type_flag, b'x' | b'X' | b'g') {
                skip_exact(&mut tar, padded_size(size)?)?;
                continue;
            }
            let full_path = pending_long_path.take().unwrap_or(header_path);
            let is_file = matches!(type_flag, 0 | b'0' | b' ' | b'7');
            if is_file && is_flash_image(&full_path) {
                entries.push(VivoFirmwareEntry {
                    name: Path::new(&full_path)
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or_default()
                        .to_string(),
                    full_path,
                    size_bytes: i64::try_from(size).unwrap_or(i64::MAX),
                });
            }
            skip_exact(&mut tar, padded_size(size)?)?;
        }
        Ok(entries)
    }

    pub fn extract(
        archive_path: &Path,
        selected: &[VivoFirmwareEntry],
        output_directory: &Path,
    ) -> Result<Vec<PayloadExtractionResult>, VivoFirmwareError> {
        Self::extract_with_cancel(archive_path, selected, output_directory, || false)
    }

    pub fn extract_with_cancel<F>(
        archive_path: &Path,
        selected: &[VivoFirmwareEntry],
        output_directory: &Path,
        is_canceled: F,
    ) -> Result<Vec<PayloadExtractionResult>, VivoFirmwareError>
    where
        F: FnMut() -> bool,
    {
        Self::extract_with_cancel_and_progress(
            archive_path,
            selected,
            output_directory,
            is_canceled,
            |_| {},
        )
    }

    pub fn extract_with_cancel_and_progress<F, P>(
        archive_path: &Path,
        selected: &[VivoFirmwareEntry],
        output_directory: &Path,
        mut is_canceled: F,
        mut report_progress: P,
    ) -> Result<Vec<PayloadExtractionResult>, VivoFirmwareError>
    where
        F: FnMut() -> bool,
        P: FnMut(VivoFirmwareProgress),
    {
        reject_duplicate_output_basenames(selected)?;
        fs::create_dir_all(output_directory)
            .map_err(|error| VivoFirmwareError::Read(error.to_string()))?;
        let wanted: HashSet<&str> = selected
            .iter()
            .map(|entry| entry.full_path.as_str())
            .collect();
        let (mut tar, gzip_stream_bytes) = open_tar(archive_path)?;
        let mut partials = Vec::new();
        let mut cleanup_paths = Vec::new();
        let mut pending_long_path = None;
        let total_bytes = selected
            .iter()
            .map(|entry| u64::try_from(entry.size_bytes).unwrap_or(0))
            .sum::<u64>();
        let mut completed_bytes = 0u64;

        let result = (|| {
            while let Some(header) = read_header(&mut tar)? {
                ensure_not_canceled(&mut is_canceled)?;
                let (header_path, size, type_flag) = parse_header(&header)?;
                if type_flag == b'L' {
                    pending_long_path = Some(read_entry_text(&mut tar, size)?);
                    skip_exact_with_cancel(
                        &mut tar,
                        padded_size(size)? - size,
                        &mut is_canceled,
                    )?;
                    continue;
                }
                if matches!(type_flag, b'x' | b'X' | b'g') {
                    skip_exact_with_cancel(&mut tar, padded_size(size)?, &mut is_canceled)?;
                    continue;
                }
                let full_path = pending_long_path.take().unwrap_or(header_path);
                let is_file = matches!(type_flag, 0 | b'0' | b' ' | b'7');
                if is_file && wanted.contains(full_path.as_str()) {
                    let name = Path::new(&full_path)
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or_default();
                    let output_path = output_directory.join(name);
                    let partial_path = output_directory.join(format!("{name}.partial"));
                    cleanup_paths.push(partial_path.clone());
                    let current_entry = full_path.clone();
                    copy_exact_with_progress(
                        &mut tar,
                        &partial_path,
                        size,
                        &mut is_canceled,
                        &gzip_stream_bytes,
                        |entry_bytes, gzip_stream_bytes| {
                            report_progress(VivoFirmwareProgress {
                                current_entry: current_entry.clone(),
                                gzip_stream_bytes,
                                entry_bytes,
                                completed_bytes: completed_bytes.saturating_add(entry_bytes),
                                total_bytes,
                            });
                        },
                    )?;
                    skip_exact_with_cancel(
                        &mut tar,
                        padded_size(size)? - size,
                        &mut is_canceled,
                    )?;
                    completed_bytes = completed_bytes.saturating_add(size);
                    partials.push((full_path, output_path, partial_path, size));
                } else {
                    let current_entry = Path::new(&full_path)
                        .file_name()
                        .and_then(|name| name.to_str())
                        .map(str::to_string);
                    skip_exact_with_cancel_and_progress(
                        &mut tar,
                        padded_size(size)?,
                        &mut is_canceled,
                        || {
                            report_progress(VivoFirmwareProgress {
                                current_entry: current_entry.clone().unwrap_or_default(),
                                gzip_stream_bytes: gzip_stream_bytes.load(Ordering::Relaxed),
                                entry_bytes: 0,
                                completed_bytes,
                                total_bytes,
                            });
                        },
                    )?;
                }
            }

            ensure_not_canceled(&mut is_canceled)?;
            let mut results = Vec::with_capacity(partials.len());
            for (full_path, output_path, partial_path, size) in &partials {
                fs::rename(partial_path, output_path)
                    .map_err(|error| VivoFirmwareError::Read(error.to_string()))?;
                results.push(PayloadExtractionResult {
                    partition_name: Path::new(full_path)
                        .file_stem()
                        .and_then(|name| name.to_str())
                        .unwrap_or_default()
                        .to_string(),
                    output_path: output_path.to_string_lossy().into_owned(),
                    size_bytes: i64::try_from(*size).unwrap_or(i64::MAX),
                });
            }
            report_progress(VivoFirmwareProgress {
                current_entry: String::new(),
                gzip_stream_bytes: gzip_stream_bytes.load(Ordering::Relaxed),
                entry_bytes: 0,
                completed_bytes: total_bytes,
                total_bytes,
            });
            Ok(results)
        })();

        if result.is_err() {
            for partial_path in &cleanup_paths {
                let _ = fs::remove_file(partial_path);
            }
        }
        result
    }
}

fn open_tar(archive_path: &Path) -> Result<(Box<dyn Read>, Arc<AtomicU64>), VivoFirmwareError> {
    let mut magic_file =
        File::open(archive_path).map_err(|error| VivoFirmwareError::Read(error.to_string()))?;
    let mut magic = [0u8; 4];
    let magic_count = magic_file
        .read(&mut magic)
        .map_err(|error| VivoFirmwareError::Read(error.to_string()))?;
    let file =
        File::open(archive_path).map_err(|error| VivoFirmwareError::Read(error.to_string()))?;
    let bytes_read = Arc::new(AtomicU64::new(0));
    let counted = CountingReader {
        inner: file,
        bytes_read: bytes_read.clone(),
    };
    let tar: Box<dyn Read> = match magic[..magic_count].starts_with(&[0x1f, 0x8b]) {
        true => Box::new(GzDecoder::new(counted)),
        false if magic_count == 4 && magic == [0x28, 0xb5, 0x2f, 0xfd] => Box::new(
            zstd::stream::read::Decoder::new(counted)
                .map_err(|error| VivoFirmwareError::Read(error.to_string()))?,
        ),
        false => {
            return Err(VivoFirmwareError::Read(
                "不支持的 VIVO 固件压缩格式。".to_string(),
            ));
        }
    };
    Ok((tar, bytes_read))
}

fn read_header(stream: &mut impl Read) -> Result<Option<[u8; 512]>, VivoFirmwareError> {
    let mut header = [0; 512];
    let mut read = 0;
    while read < header.len() {
        let count = stream
            .read(&mut header[read..])
            .map_err(|error| VivoFirmwareError::Read(error.to_string()))?;
        if count == 0 {
            if read == 0 {
                return Ok(None);
            }
            return Err(VivoFirmwareError::Truncated("tar header".to_string()));
        }
        read += count;
    }
    if header.iter().all(|value| *value == 0) {
        return Ok(None);
    }
    Ok(Some(header))
}

fn parse_header(header: &[u8; 512]) -> Result<(String, u64, u8), VivoFirmwareError> {
    let full_path = header_text(&header[..100]);
    let size = parse_octal(&header[124..136])?;
    let type_flag = header[156];
    Ok((full_path, size, type_flag))
}

fn read_entry_text(stream: &mut impl Read, size: u64) -> Result<String, VivoFirmwareError> {
    let mut bytes = vec![
        0;
        usize::try_from(size).map_err(|_| {
            VivoFirmwareError::Read("tar 长文件名长度超出支持范围。".to_string())
        })?
    ];
    read_exact(stream, &mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes)
        .trim_end_matches('\0')
        .to_string())
}

fn parse_octal(bytes: &[u8]) -> Result<u64, VivoFirmwareError> {
    if bytes.first().is_some_and(|value| value & 0x80 != 0) {
        let mut value = 0u64;
        for (index, byte) in bytes.iter().enumerate() {
            value = (value << 8) | u64::from(if index == 0 { byte & 0x7f } else { *byte });
        }
        return Ok(value);
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|error| VivoFirmwareError::Read(error.to_string()))?
        .trim_matches(['\0', ' ']);
    if text.is_empty() {
        return Ok(0);
    }
    u64::from_str_radix(text, 8).map_err(|error| VivoFirmwareError::Read(error.to_string()))
}

fn header_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .trim_end_matches('\0')
        .trim()
        .to_string()
}

/// Rounds a tar entry size up to the 512-byte header block grid. A hostile
/// header can declare a size near u64::MAX, so overflow is surfaced as a
/// truncation error instead of silently wrapping the skip offset.
fn padded_size(size: u64) -> Result<u64, VivoFirmwareError> {
    let padding = (512 - (size % 512)) % 512;
    size.checked_add(padding)
        .ok_or_else(|| VivoFirmwareError::Truncated("tar entry size".to_string()))
}

fn skip_exact(stream: &mut impl Read, remaining: u64) -> Result<(), VivoFirmwareError> {
    skip_exact_with_cancel(stream, remaining, &mut || false)
}

fn skip_exact_with_cancel(
    stream: &mut impl Read,
    mut remaining: u64,
    is_canceled: &mut impl FnMut() -> bool,
) -> Result<(), VivoFirmwareError> {
    let mut buffer = [0; STREAM_BUFFER_SIZE];
    while remaining > 0 {
        ensure_not_canceled(is_canceled)?;
        let requested = remaining.min(STREAM_BUFFER_SIZE as u64) as usize;
        let count = stream
            .read(&mut buffer[..requested])
            .map_err(|error| VivoFirmwareError::Read(error.to_string()))?;
        if count == 0 {
            return Err(VivoFirmwareError::Truncated("tar entry".to_string()));
        }
        remaining -= count as u64;
    }
    Ok(())
}

fn skip_exact_with_cancel_and_progress(
    stream: &mut impl Read,
    mut remaining: u64,
    is_canceled: &mut impl FnMut() -> bool,
    mut report_progress: impl FnMut(),
) -> Result<(), VivoFirmwareError> {
    let mut buffer = [0; STREAM_BUFFER_SIZE];
    while remaining > 0 {
        ensure_not_canceled(is_canceled)?;
        let requested = remaining.min(STREAM_BUFFER_SIZE as u64) as usize;
        let count = stream
            .read(&mut buffer[..requested])
            .map_err(|error| VivoFirmwareError::Read(error.to_string()))?;
        if count == 0 {
            return Err(VivoFirmwareError::Truncated("tar entry".to_string()));
        }
        remaining -= count as u64;
        report_progress();
    }
    Ok(())
}

fn is_flash_image(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("img") || extension.eq_ignore_ascii_case("bin")
        })
}

fn reject_duplicate_output_basenames(
    selected: &[VivoFirmwareEntry],
) -> Result<(), VivoFirmwareError> {
    let mut names = HashSet::new();
    for entry in selected {
        let name = Path::new(&entry.full_path)
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .ok_or_else(|| VivoFirmwareError::Read("固件条目名称无效。".to_string()))?
            .to_ascii_lowercase();
        if !names.insert(name) {
            return Err(VivoFirmwareError::Read(
                "选择的固件分区存在重名输出。".to_string(),
            ));
        }
    }
    Ok(())
}

fn read_exact(stream: &mut impl Read, bytes: &mut [u8]) -> Result<(), VivoFirmwareError> {
    let mut offset = 0;
    while offset < bytes.len() {
        let count = stream
            .read(&mut bytes[offset..])
            .map_err(|error| VivoFirmwareError::Read(error.to_string()))?;
        if count == 0 {
            return Err(VivoFirmwareError::Truncated("tar entry".to_string()));
        }
        offset += count;
    }
    Ok(())
}

fn copy_exact_with_progress(
    stream: &mut impl Read,
    output_path: &Path,
    mut remaining: u64,
    is_canceled: &mut impl FnMut() -> bool,
    gzip_stream_bytes: &AtomicU64,
    mut report_progress: impl FnMut(u64, u64),
) -> Result<(), VivoFirmwareError> {
    let mut output =
        File::create(output_path).map_err(|error| VivoFirmwareError::Read(error.to_string()))?;
    let mut buffer = [0; STREAM_BUFFER_SIZE];
    let mut copied = 0u64;
    while remaining > 0 {
        ensure_not_canceled(is_canceled)?;
        let requested = remaining.min(STREAM_BUFFER_SIZE as u64) as usize;
        let count = stream
            .read(&mut buffer[..requested])
            .map_err(|error| VivoFirmwareError::Read(error.to_string()))?;
        if count == 0 {
            return Err(VivoFirmwareError::Truncated("tar entry".to_string()));
        }
        output
            .write_all(&buffer[..count])
            .map_err(|error| VivoFirmwareError::Read(error.to_string()))?;
        remaining -= count as u64;
        copied = copied.saturating_add(count as u64);
        report_progress(copied, gzip_stream_bytes.load(Ordering::Relaxed));
    }
    output
        .sync_all()
        .map_err(|error| VivoFirmwareError::Read(error.to_string()))
}

fn ensure_not_canceled(is_canceled: &mut impl FnMut() -> bool) -> Result<(), VivoFirmwareError> {
    if is_canceled() {
        return Err(VivoFirmwareError::Canceled);
    }
    Ok(())
}
