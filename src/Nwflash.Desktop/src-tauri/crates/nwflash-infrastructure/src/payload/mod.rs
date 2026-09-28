//! Android OTA payload（CrAU）解析与提取。
//!
//! 按 AOSP `update_engine` 的公开格式实现：
//! * 头魔法 `CrAU` + version(u64) + manifest_size(u64) + metadata_signature_size(u32)
//! * manifest 是 protobuf（`DeltaArchiveManifest`），用手写 varint 解析
//! * 每个 operation 的 data 按 `dst_extents` 写入输出镜像的稀疏位置
//!
//! **为什么自己做而不是调外部工具**：进度必须是真实的。外部工具把解压循环
//! 关在自己进程里，父进程只能靠轮询产物文件大小猜进度；而 `payload_dumper`
//! 一上来就 `set_len` 把文件撑到最终大小，轮询到的永远是「0% 或 100%」。
//! 解法在循环内部才有——[`Payload::extract_partitions`] 每写完一块就回调，
//! 拿到的是货真价实的字节数。
//!
//! 入口：
//! * [`Payload::parse`] —— 本地文件
//! * [`Payload::from_reader`] —— 任意 `Read + Seek`（含 [`remote::RemoteReader`]）

pub mod remote;

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use thiserror::Error;

/// payload 处理的失败原因。
#[derive(Debug, Error)]
pub enum PayloadError {
    #[error("payload 缺少 CrAU 魔数。")]
    MissingMagic,
    #[error("不支持的 payload 版本 {0}，仅支持 version 2。")]
    UnsupportedVersion(u64),
    #[error("payload 数据损坏：{0}")]
    Corrupt(String),
    #[error("payload 使用了差分操作，暂不支持。")]
    DifferentialUnsupported,
    #[error("不支持的 operation 类型 {0}。")]
    UnsupportedOperation(u64),
    #[error("payload 中不存在分区 {0}。")]
    MissingPartition(String),
    #[error("payload 提取已取消。")]
    Canceled,
    #[error("读取或写入 payload 时发生 I/O 错误：{0}")]
    Io(#[from] io::Error),
}

/// 单个 extent：从 start_block 起连续 num_blocks 个块。
#[derive(Debug, Clone, Copy)]
pub struct Extent {
    pub start_block: u64,
    pub num_blocks: u64,
}

#[derive(Debug, Clone)]
pub struct Operation {
    pub op_type: u64,
    pub data_offset: u64,
    pub data_length: u64,
    pub dst_extents: Vec<Extent>,
}

#[derive(Debug, Clone)]
pub struct Partition {
    pub name: String,
    pub new_size: u64,
    pub operations: Vec<Operation>,
}

#[derive(Debug, Clone)]
pub struct Manifest {
    pub block_size: u32,
    pub partitions: Vec<Partition>,
}

/// 单个分区的提取结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedPartition {
    pub name: String,
    pub output_path: std::path::PathBuf,
    /// 实际写入的字节数（解压后）。
    pub bytes_written: u64,
    /// manifest 声明的分区大小。
    pub total_bytes: u64,
}

pub struct Payload<R> {
    pub manifest: Manifest,
    /// 数据区起始偏移（头 + manifest + signature 之后，已含 `base`）。
    pub data_offset: u64,
    source: R,
    source_len: u64,
}

/// 手写 `Debug`：`Payload` 可能持有网络读取器或文件句柄，那些类型未必实现
/// `Debug`，但我们只需要在测试的 `expect_err` 里打印摘要。
impl<R> std::fmt::Debug for Payload<R> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Payload")
            .field("block_size", &self.manifest.block_size)
            .field("partitions", &self.manifest.partitions.len())
            .field("data_offset", &self.data_offset)
            .field("source_len", &self.source_len)
            .finish()
    }
}

/// 手写 protobuf 读取器。只实现 payload manifest 用到的 wire type。
struct ProtoReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> ProtoReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn has_next(&self) -> bool {
        self.pos < self.data.len()
    }

    fn read_varint(&mut self) -> io::Result<u64> {
        let mut result: u64 = 0;
        let mut shift = 0;
        loop {
            if self.pos >= self.data.len() {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "varint eof"));
            }
            let byte = self.data[self.pos];
            self.pos += 1;
            result |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(result);
            }
            shift += 7;
            if shift >= 64 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "varint too long",
                ));
            }
        }
    }

    fn read_tag(&mut self) -> io::Result<u32> {
        Ok(self.read_varint()? as u32)
    }

    fn read_length_delimited(&mut self) -> io::Result<&'a [u8]> {
        let len = self.read_varint()? as usize;
        // 长度字段来自文件内容：溢出或越界都按损坏处理，绝不绕过切片边界。
        let end = self.pos.checked_add(len).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "length delimited overflow")
        })?;
        if end > self.data.len() {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "ld eof"));
        }
        let out = &self.data[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    fn read_string(&mut self) -> io::Result<String> {
        Ok(String::from_utf8_lossy(self.read_length_delimited()?).into_owned())
    }

    /// 跳过未知字段，保证向后兼容（新版本 manifest 加字段不会炸）。
    fn skip_field(&mut self, wire: u32) -> io::Result<()> {
        match wire {
            0 => {
                self.read_varint()?;
            }
            1 => self.pos += 8,
            2 => {
                self.read_length_delimited()?;
            }
            5 => self.pos += 4,
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unsupported wire type {other}"),
                ))
            }
        }
        if self.pos > self.data.len() {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "skip eof"));
        }
        Ok(())
    }
}

impl Payload<File> {
    /// 打开本地 payload 文件。
    pub fn parse(path: &Path) -> Result<Self, PayloadError> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        Self::from_reader(file, len)
    }
}

impl<R: Read + Seek> Payload<R> {
    /// 从任意 `Read + Seek` 源解析：本地文件、内存缓冲、或远程 Range 读取器。
    ///
    /// 远程场景下 `source_len` 是服务器报告的完整长度，`Seek` 直接落到数据区
    /// 起始处读头与 manifest，不会把整个固件拉下来。
    pub fn from_reader(source: R, source_len: u64) -> Result<Self, PayloadError> {
        Self::from_reader_at(source, source_len, 0)
    }

    /// 同 [`Self::from_reader`]，但 CrAU 数据不在源的开头（例如 zip 内成员）。
    ///
    /// `base` 是 CrAU 魔数在源中的绝对偏移；返回的 `data_offset` 也是绝对偏移，
    /// 因此后续 `extract_partition` 无需知道 zip 的存在。
    pub fn from_reader_at(mut source: R, source_len: u64, base: u64) -> Result<Self, PayloadError> {
        source.seek(SeekFrom::Start(base))?;

        let mut head = [0u8; 24];
        source.read_exact(&mut head)?;
        if &head[0..4] != b"CrAU" {
            return Err(PayloadError::MissingMagic);
        }
        let version = u64::from_be_bytes(head[4..12].try_into().unwrap());
        if version != 2 {
            return Err(PayloadError::UnsupportedVersion(version));
        }
        let manifest_size = u64::from_be_bytes(head[12..20].try_into().unwrap());
        let signature_size = u32::from_be_bytes(head[20..24].try_into().unwrap());

        // manifest 大小受三重约束：头部长度字段不溢出、不越过源末尾、不超过
        // 硬上限。这是文件内容可控的字段，直接 `vec![0u8; size]` 会把损坏
        // 文件的 8 个字节变成一次无界内存分配——分配失败是进程级 abort，
        // 不像普通错误那样可以恢复。
        const MAX_MANIFEST_BYTES: u64 = 64 * 1024 * 1024;
        let manifest_end = 24u64
            .checked_add(manifest_size)
            .and_then(|size| size.checked_add(u64::from(signature_size)))
            .ok_or_else(|| PayloadError::Corrupt("payload 头部长度字段溢出。".to_string()))?;
        if manifest_size > MAX_MANIFEST_BYTES {
            return Err(PayloadError::Corrupt(format!(
                "payload manifest 大小异常（{manifest_size} 字节，超过 {MAX_MANIFEST_BYTES} 上限）。"
            )));
        }
        if base
            .checked_add(manifest_end)
            .is_none_or(|end| end > source_len)
        {
            return Err(PayloadError::Corrupt(
                "payload 头部声明的 manifest 越出文件范围。".to_string(),
            ));
        }
        let manifest_len = usize::try_from(manifest_size)
            .map_err(|_| PayloadError::Corrupt("payload manifest 大小异常。".to_string()))?;

        // manifest 可能很大（数 MB），但要按 manifest_size 精确读取，
        // 不能一次吞掉整块——远程源每次 Read 都是一次 Range 请求。
        let manifest_bytes = read_exact_vec(&mut source, manifest_len)?;
        let data_offset = base + 24 + manifest_size + u64::from(signature_size);
        let manifest = parse_manifest(&manifest_bytes)?;
        Ok(Self {
            manifest,
            data_offset,
            source,
            source_len,
        })
    }

    /// 按 extent 把数据写入输出镜像，并在每写入一块后回调进度。
    ///
    /// `on_progress` 的参数是「本次 operation 已写入的字节数」。
    pub fn extract_partition<F>(
        &mut self,
        name: &str,
        output: &Path,
        mut on_progress: F,
    ) -> Result<u64, PayloadError>
    where
        F: FnMut(u64),
    {
        let dir = output.parent().unwrap_or_else(|| Path::new("."));
        let results = self.extract_partitions_with_cancel(
            &[name],
            dir,
            |_, written, _| on_progress(written),
            || false,
        )?;
        Ok(results.into_iter().next().map_or(0, |r| r.bytes_written))
    }

    /// 依次提取多个分区到 `output_dir`，每个分区的进度单独回调。
    ///
    /// 回调参数为 `(分区名, 该分区已写入字节, 该分区总大小)`。总大小来自
    /// manifest 的 `new_partition_info.size`，所以调用方**不需要**事先知道
    /// 分区尺寸——这正是旧实现做不到的（它要把尺寸传进来才能算百分比）。
    ///
    /// 不需要取消的调用方用 [`Self::extract_partitions`]；提取可能耗时
    /// 很长（远程固件解压数 GB），主流程必须能在中途停止。
    pub fn extract_partitions<F>(
        &mut self,
        names: &[&str],
        output_dir: &Path,
        on_progress: F,
    ) -> Result<Vec<ExtractedPartition>, PayloadError>
    where
        F: FnMut(&str, u64, u64),
    {
        self.extract_partitions_with_cancel(names, output_dir, on_progress, || false)
    }

    /// 同 [`Self::extract_partitions`]，但带取消检查点。
    ///
    /// 检查点分布在：每个分区开始前、每个 operation 之间、数据拷贝循环的
    /// 每一块（256 KiB）与写零循环的每一块。命中取消即返回
    /// [`PayloadError::Canceled`]，已写的临时文件由调用方清理。
    pub fn extract_partitions_with_cancel<F, C>(
        &mut self,
        names: &[&str],
        output_dir: &Path,
        mut on_progress: F,
        mut is_canceled: C,
    ) -> Result<Vec<ExtractedPartition>, PayloadError>
    where
        F: FnMut(&str, u64, u64),
        C: FnMut() -> bool,
    {
        fs::create_dir_all(output_dir)?;
        let mut results = Vec::with_capacity(names.len());
        for name in names {
            if is_canceled() {
                return Err(PayloadError::Canceled);
            }
            ensure_safe_partition_name(name)?;
            let partition = self
                .manifest
                .partitions
                .iter()
                .find(|p| p.name == *name)
                .ok_or_else(|| PayloadError::MissingPartition((*name).to_string()))?
                .clone();

            let output = output_dir.join(format!("{}.img", partition.name));
            let mut out = File::create(&output)?;
            // 预分配最终大小：sparse extent 写入需要文件已够长，且能避免碎片。
            out.set_len(partition.new_size)?;

            let total = partition.new_size;
            let mut written_total: u64 = 0;
            for op in &partition.operations {
                if is_canceled() {
                    return Err(PayloadError::Canceled);
                }
                let name = partition.name.clone();
                let mut tick = |written: u64| {
                    on_progress(&name, written_total + written, total);
                };
                let written = self.extract_operation(op, &mut out, &mut is_canceled, &mut tick)?;
                written_total += written;
            }
            out.sync_all()?;
            results.push(ExtractedPartition {
                name: partition.name,
                output_path: output,
                bytes_written: written_total,
                total_bytes: total,
            });
        }
        Ok(results)
    }

    fn extract_operation<C, F>(
        &mut self,
        op: &Operation,
        out: &mut File,
        is_canceled: &mut C,
        on_progress: &mut F,
    ) -> Result<u64, PayloadError>
    where
        C: FnMut() -> bool,
        F: FnMut(u64),
    {
        // op_type: 0=REPLACE, 1=REPLACE_BZ, 6=ZERO, 8=REPLACE_XZ, 14=ZSTD
        self.source
            .seek(SeekFrom::Start(self.data_offset + op.data_offset))?;
        let mut reader = (&mut self.source).take(op.data_length);

        let block_size = self.manifest.block_size;
        let mut sink = ExtentWriter::new(out, &op.dst_extents, block_size);
        let written = match op.op_type {
            0 => copy_with_progress(&mut reader, &mut sink, is_canceled, on_progress)
                .map_err(map_copy_failure)?,
            8 => {
                let mut decoder = liblzma::read::XzDecoder::new(reader);
                copy_with_progress(&mut decoder, &mut sink, is_canceled, on_progress)
                    .map_err(map_copy_failure)?
            }
            1 => {
                let mut decoder = bzip2::read::BzDecoder::new(reader);
                copy_with_progress(&mut decoder, &mut sink, is_canceled, on_progress)
                    .map_err(map_copy_failure)?
            }
            14 => {
                // ZSTD（Android 12+ 的 OTA 大量使用）——本项目的真实固件就是这种。
                let mut decoder = zstd::stream::read::Decoder::new(reader)?;
                copy_with_progress(&mut decoder, &mut sink, is_canceled, on_progress)
                    .map_err(map_copy_failure)?
            }
            6 => {
                // AOSP 的 ZERO 操作没有数据负载（data_length 通常为 0），
                // 输出长度由 dst_extents 决定；个别生成器会把长度写进
                // data_length。优先用 extents 总量，退化才用 data_length——
                // 按 extents 写零还能借 ExtentWriter 的容量校验发现清单
                // 与声明不一致。
                let extent_total: u64 = op
                    .dst_extents
                    .iter()
                    .map(|extent| extent.num_blocks * u64::from(block_size))
                    .sum();
                let mut remaining = if extent_total > 0 {
                    extent_total
                } else {
                    op.data_length
                };
                let zeros = vec![0u8; 64 * 1024];
                let mut written_zero: u64 = 0;
                while remaining > 0 {
                    if is_canceled() {
                        return Err(PayloadError::Canceled);
                    }
                    let chunk = remaining.min(zeros.len() as u64) as usize;
                    sink.write_all(&zeros[..chunk])?;
                    remaining -= chunk as u64;
                    written_zero += chunk as u64;
                    on_progress(written_zero);
                }
                written_zero
            }
            other => {
                return Err(PayloadError::UnsupportedOperation(other));
            }
        };
        sink.flush()?;
        Ok(written)
    }

    pub fn source_len(&self) -> u64 {
        self.source_len
    }
}

/// 精确读取 `len` 字节。远程源上每次 Read 都是一次 Range 请求，
/// 所以不能用「一次大 buf」的写法碰运气，必须循环读满。
fn read_exact_vec<R: Read>(reader: &mut R, len: usize) -> io::Result<Vec<u8>> {
    let mut buffer = vec![0u8; len];
    let mut filled = 0usize;
    while filled < len {
        let read = reader.read(&mut buffer[filled..])?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "payload manifest 提前结束",
            ));
        }
        filled += read;
    }
    Ok(buffer)
}

/// 数据拷贝循环的失败：取消与 I/O 是两种不同性质的结果，不能共用
/// `io::Error`（`ErrorKind::Interrupted` 在标准库里是 EINTR 的正常重试
/// 信号，挪用它表达「用户取消」会把真实的系统中断误报成取消）。
enum CopyFailure {
    Canceled,
    Io(io::Error),
}

fn map_copy_failure(failure: CopyFailure) -> PayloadError {
    match failure {
        CopyFailure::Canceled => PayloadError::Canceled,
        CopyFailure::Io(error) => PayloadError::Io(error),
    }
}

fn copy_with_progress<R, W, C, F>(
    reader: &mut R,
    writer: &mut W,
    is_canceled: &mut C,
    on_progress: &mut F,
) -> Result<u64, CopyFailure>
where
    R: Read,
    W: Write,
    C: FnMut() -> bool,
    F: FnMut(u64),
{
    let mut buffer = vec![0u8; 256 * 1024];
    let mut total: u64 = 0;
    loop {
        if is_canceled() {
            return Err(CopyFailure::Canceled);
        }
        let read = reader.read(&mut buffer).map_err(CopyFailure::Io)?;
        if read == 0 {
            break;
        }
        writer.write_all(&buffer[..read]).map_err(CopyFailure::Io)?;
        total += read as u64;
        on_progress(total);
    }
    Ok(total)
}

/// 分区名要拼成 `{name}.img` 落盘。名字来自 manifest——即固件文件本身的
/// 内容——带路径语义的名字会把写盘位置移出输出目录，一律拒绝。
fn ensure_safe_partition_name(name: &str) -> Result<(), PayloadError> {
    let dangerous =
        name.is_empty() || name.contains(['/', '\\', ':', '\0']) || name == "." || name == "..";
    if dangerous {
        Err(PayloadError::Corrupt(format!(
            "分区名 {name:?} 含路径语义，拒绝写盘。"
        )))
    } else {
        Ok(())
    }
}

/// 按 extent 列表把连续字节流写到文件的不同偏移。
///
/// 镜像里相邻 extent 之间可能有空洞（未写入区），所以不能顺序写，
/// 必须按 extent 逐个 seek。
struct ExtentWriter<'a> {
    out: &'a mut File,
    extents: &'a [Extent],
    block_size: u32,
    index: usize,
    offset_in_extent: u64,
}

impl<'a> ExtentWriter<'a> {
    fn new(out: &'a mut File, extents: &'a [Extent], block_size: u32) -> Self {
        Self {
            out,
            extents,
            block_size,
            index: 0,
            offset_in_extent: 0,
        }
    }

    fn seek_current(&mut self) -> io::Result<()> {
        if let Some(extent) = self.extents.get(self.index) {
            let position = extent.start_block * u64::from(self.block_size) + self.offset_in_extent;
            self.out.seek(SeekFrom::Start(position))?;
        }
        Ok(())
    }
}

impl Write for ExtentWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut cursor = 0;
        let mut remaining = buf.len();
        while remaining > 0 {
            let Some(extent) = self.extents.get(self.index) else {
                // 解压出的字节比 manifest 声明的 extent 总量还多 —— 数据不可信，
                // 宁可失败也不要把多余的字节悄悄丢掉。
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "解压数据超出 manifest 声明的 extent 范围",
                ));
            };
            let capacity = extent.num_blocks * u64::from(self.block_size);
            let available = capacity.saturating_sub(self.offset_in_extent);
            if available == 0 {
                self.index += 1;
                self.offset_in_extent = 0;
                self.seek_current()?;
                continue;
            }
            let chunk = remaining.min(available as usize);
            self.out.write_all(&buf[cursor..cursor + chunk])?;
            cursor += chunk;
            remaining -= chunk;
            self.offset_in_extent += chunk as u64;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

fn parse_manifest(bytes: &[u8]) -> io::Result<Manifest> {
    let mut reader = ProtoReader::new(bytes);
    let mut manifest = Manifest {
        block_size: 4096,
        partitions: Vec::new(),
    };
    while reader.has_next() {
        let tag = reader.read_tag()?;
        let field = tag >> 3;
        let wire = tag & 0x7;
        match (field, wire) {
            // DeltaArchiveManifest.block_size = 3
            (3, 0) => manifest.block_size = reader.read_varint()? as u32,
            // DeltaArchiveManifest.partitions = 13
            (13, 2) => {
                let inner = reader.read_length_delimited()?;
                manifest.partitions.push(parse_partition(inner)?);
            }
            _ => reader.skip_field(wire)?,
        }
    }
    Ok(manifest)
}

fn parse_partition(bytes: &[u8]) -> io::Result<Partition> {
    let mut reader = ProtoReader::new(bytes);
    let mut partition = Partition {
        name: String::new(),
        new_size: 0,
        operations: Vec::new(),
    };
    while reader.has_next() {
        let tag = reader.read_tag()?;
        let field = tag >> 3;
        let wire = tag & 0x7;
        match (field, wire) {
            // PartitionUpdate.partition_name = 1
            (1, 2) => partition.name = reader.read_string()?,
            // PartitionUpdate.operations = 8
            (8, 2) => {
                let inner = reader.read_length_delimited()?;
                partition.operations.push(parse_operation(inner)?);
            }
            // PartitionUpdate.new_partition_info = 7
            (7, 2) => {
                let inner = reader.read_length_delimited()?;
                partition.new_size = parse_partition_info_size(inner)?;
            }
            _ => reader.skip_field(wire)?,
        }
    }
    Ok(partition)
}

/// PartitionInfo.size = 1
fn parse_partition_info_size(bytes: &[u8]) -> io::Result<u64> {
    let mut reader = ProtoReader::new(bytes);
    let mut size = 0;
    while reader.has_next() {
        let tag = reader.read_tag()?;
        let field = tag >> 3;
        let wire = tag & 0x7;
        match (field, wire) {
            (1, 0) => size = reader.read_varint()?,
            _ => reader.skip_field(wire)?,
        }
    }
    Ok(size)
}

fn parse_operation(bytes: &[u8]) -> io::Result<Operation> {
    let mut reader = ProtoReader::new(bytes);
    let mut op = Operation {
        op_type: 0,
        data_offset: 0,
        data_length: 0,
        dst_extents: Vec::new(),
    };
    while reader.has_next() {
        let tag = reader.read_tag()?;
        let field = tag >> 3;
        let wire = tag & 0x7;
        match (field, wire) {
            // InstallOperation.type = 1
            (1, 0) => op.op_type = reader.read_varint()?,
            // InstallOperation.data_offset = 2
            (2, 0) => op.data_offset = reader.read_varint()?,
            // InstallOperation.data_length = 3
            (3, 0) => op.data_length = reader.read_varint()?,
            // InstallOperation.dst_extents = 6
            (6, 2) => {
                let inner = reader.read_length_delimited()?;
                op.dst_extents.push(parse_extent(inner)?);
            }
            _ => reader.skip_field(wire)?,
        }
    }
    Ok(op)
}

fn parse_extent(bytes: &[u8]) -> io::Result<Extent> {
    let mut reader = ProtoReader::new(bytes);
    let mut extent = Extent {
        start_block: 0,
        num_blocks: 0,
    };
    while reader.has_next() {
        let tag = reader.read_tag()?;
        let field = tag >> 3;
        let wire = tag & 0x7;
        match (field, wire) {
            // Extent.start_block = 1
            (1, 0) => extent.start_block = reader.read_varint()?,
            // Extent.num_blocks = 2
            (2, 0) => extent.num_blocks = reader.read_varint()?,
            _ => reader.skip_field(wire)?,
        }
    }
    Ok(extent)
}

pub fn default_output_path(name: &str) -> PathBuf {
    PathBuf::from(format!("{name}.img"))
}
