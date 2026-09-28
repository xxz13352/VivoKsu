//! 远程 payload 直读：从 URL 按 Range 读取 CrAU 数据，不下载整包。
//!
//! 复现思路（不是复制代码）：现有的 `RangeHttpReader` 已经把「Vivo CDN 长 Range
//! 断流」处理好了，这里只做两件事：
//! 1. 探测 URL 是裸 payload 还是 zip 包着 payload.bin；
//! 2. 把「从 URL 的某个偏移开始读」包装成一个 `Read + Seek`，交给解析器。
//!
//! 关键：解析器只需要 `Read + Seek`，所以远程和本地走同一条代码路径，
//! 区别仅在于 `Seek` 的实现是发 Range 请求还是改文件指针。

use std::io::{self, Read, Seek, SeekFrom};

/// 远程源的最小抽象：支持按偏移读取。
///
/// 抽成 trait 是为了让解析器不依赖具体的 HTTP 客户端，测试可以塞内存实现。
pub trait RemoteRead {
    /// 读取 `buf`，返回实际读到的字节数；0 表示 EOF。
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<usize>;

    /// 远端资源总长度。
    fn total_len(&self) -> u64;
}

/// 把按偏移读取的源适配成 `Read + Seek`，供 [`crate::Payload::from_reader`] 使用。
pub struct RemoteReader<S> {
    source: S,
    position: u64,
    len: u64,
}

impl<S: RemoteRead> RemoteReader<S> {
    pub fn new(source: S) -> Self {
        let len = source.total_len();
        Self {
            source,
            position: 0,
            len,
        }
    }

    /// 底层源的总长度（与 `Seek` 的末端一致）。
    pub fn source_total_len(&self) -> u64 {
        self.len
    }
}

impl<S: RemoteRead> Read for RemoteReader<S> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || self.position >= self.len {
            return Ok(0);
        }
        let read = self.source.read_at(self.position, buf)?;
        self.position += read as u64;
        Ok(read)
    }
}

impl<S: RemoteRead> Seek for RemoteReader<S> {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let target = match from {
            SeekFrom::Start(offset) => offset as i64,
            SeekFrom::End(offset) => self.len as i64 + offset,
            SeekFrom::Current(offset) => self.position as i64 + offset,
        };
        if target < 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "seek 到负偏移"));
        }
        self.position = target as u64;
        Ok(self.position)
    }
}

/// zip 里 payload.bin 的位置信息。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PayloadLocation {
    /// payload.bin 数据的起始偏移。
    ///
    /// 仅当定位时本地头恰好落在抓取窗口内才直接给出；否则为 `None`，
    /// 调用方需用 [`PayloadLocation::data_offset`] 补读本地头后再算。
    pub offset: Option<u64>,
    /// payload.bin 的长度（来自中央目录的 ZIP64 真值）。
    pub length: u64,
    /// payload.bin 本地头的绝对偏移（相对 zip 起点）。
    pub local_header_offset: u64,
}

impl PayloadLocation {
    /// 补读本地头后算出数据起始偏移。
    ///
    /// `local_header` 必须是本地头的前 30 字节（不足则报错）。
    pub fn data_offset(&self, local_header: &[u8]) -> io::Result<u64> {
        if let Some(offset) = self.offset {
            return Ok(offset);
        }
        if local_header.len() < 30 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "payload.bin 本地头不足 30 字节",
            ));
        }
        if &local_header[0..4] != b"PK" {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "payload.bin 本地头签名不符",
            ));
        }
        let name_len = u16::from_le_bytes([local_header[26], local_header[27]]) as u64;
        let extra_len = u16::from_le_bytes([local_header[28], local_header[29]]) as u64;
        Ok(self.local_header_offset + 30 + name_len + extra_len)
    }
}

/// 在 zip 数据里定位唯一名为 `payload.bin` 的成员。
///
/// 用中央目录而不是本地头：本地头里的 size 在 ZIP64 下可能是 `0xFFFFFFFF`
/// 占位符（实测 Vivo 固件就是这样），中央目录的 ZIP64 extra 字段才有真值。
///
/// `zip_tail` 是 zip 的**末尾窗口**，`zip_base_offset` 是该窗口在远端资源中的
/// 绝对起始偏移。中央目录的偏移是相对 zip 起点的，所以要先换算成窗口内下标。
pub fn locate_payload_in_zip(zip_tail: &[u8], zip_base_offset: u64) -> io::Result<PayloadLocation> {
    let eocd = find_signature(zip_tail, b"PK\x05\x06")
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "zip 缺少 EOCD"))?;

    let mut central_offset = read_u32(zip_tail, eocd + 16)? as u64;
    let mut central_size = read_u32(zip_tail, eocd + 12)? as u64;

    // ZIP64：字段是 0xFFFFFFFF 时真值在同为 ZIP64 的尾部记录里。
    if central_offset == 0xFFFF_FFFF || central_size == 0xFFFF_FFFF {
        let z64 = find_signature(zip_tail, b"PK\x06\x06").ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "zip 缺少 ZIP64 EOCD 记录")
        })?;
        central_size = read_u64(zip_tail, z64 + 40)?;
        central_offset = read_u64(zip_tail, z64 + 48)?;
    }

    // 中央目录偏移是相对 zip 起点的绝对量；换算成窗口内下标才能切片。
    let Some(central_start) = central_offset.checked_sub(zip_base_offset) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "中央目录位于抓取窗口之前",
        ));
    };
    let central_end = central_start
        .checked_add(central_size)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "中央目录长度溢出"))?;
    if central_end > zip_tail.len() as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "抓取的尾部窗口未覆盖完整中央目录",
        ));
    }

    let central = &zip_tail[central_start as usize..central_end as usize];
    let mut cursor = 0usize;
    let mut found: Option<PayloadLocation> = None;
    while cursor + 46 <= central.len() {
        if &central[cursor..cursor + 4] != b"PK\x01\x02" {
            break;
        }
        let name_len = read_u16(central, cursor + 28)? as usize;
        let extra_len = read_u16(central, cursor + 30)? as usize;
        let comment_len = read_u16(central, cursor + 32)? as usize;
        let name_start = cursor + 46;
        let name = &central[name_start..name_start + name_len];

        let mut compressed_size = read_u32(central, cursor + 20)? as u64;
        let uncompressed_size = read_u32(central, cursor + 24)? as u64;
        let mut local_header_offset = read_u32(central, cursor + 42)? as u64;
        let extra_start = name_start + name_len;
        let extra = &central[extra_start..extra_start + extra_len];

        // ZIP64 extra（id=0x0001）：字段**顺序固定**为「原始大小 → 压缩大小 →
        // 头偏移 → 磁盘号」，且只有对应基字段是 0xFFFFFFFF 占位符的字段才
        // 出现。必须按占位符序列逐个消费，不能跳过前面的字段直接读后面的。
        if compressed_size == 0xFFFF_FFFF
            || uncompressed_size == 0xFFFF_FFFF
            || local_header_offset == 0xFFFF_FFFF
        {
            let mut pos = 0usize;
            while pos + 4 <= extra.len() {
                let id = read_u16(extra, pos)?;
                let size = read_u16(extra, pos + 2)? as usize;
                let body_start = pos + 4;
                if id == 0x0001 {
                    let mut field = body_start;
                    if uncompressed_size == 0xFFFF_FFFF {
                        // 原始大小在最前。解析器用不到它的值，但必须按规范
                        // 消费掉这 8 字节，否则后面字段的读取位置全错。
                        let _original_size = read_u64(extra, field)?;
                        field += 8;
                    }
                    if compressed_size == 0xFFFF_FFFF {
                        compressed_size = read_u64(extra, field)?;
                        field += 8;
                    }
                    if local_header_offset == 0xFFFF_FFFF {
                        local_header_offset = read_u64(extra, field)?;
                    }
                    break;
                }
                pos = body_start + size;
            }
        }

        if name == b"payload.bin" {
            if found.is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "zip 中存在多个 payload.bin",
                ));
            }
            // 只记录「本地头在哪」，先不算数据偏移。
            //
            // 数据起始 = 本地头偏移 + 30 + 本地头里的文件名长度 + 本地头里的
            // extra 长度。后两者**只能**从本地头本身读：中央目录里那份 extra
            // 是它自己的，长度不一定与本地头相同（实测两者都是 20，但不能假设）。
            // 而本地头往往不在「文件尾部窗口」内（payload.bin 是首个条目时
            // lho=0，窗口取的是尾部），所以交给调用方按需补读那 30 字节。
            found = Some(PayloadLocation {
                offset: None,
                length: compressed_size,
                local_header_offset,
            });
        }

        cursor = name_start + name_len + extra_len + comment_len;
    }

    found.ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "zip 中未找到 payload.bin"))
}

/// 定位子切片中最后一个指定签名的位置（EOCD 可能被注释体干扰）。
fn find_signature(data: &[u8], signature: &[u8]) -> Option<usize> {
    if data.len() < signature.len() {
        return None;
    }
    (0..=data.len() - signature.len())
        .rev()
        .find(|&i| &data[i..i + signature.len()] == signature)
}

fn read_u16(data: &[u8], offset: usize) -> io::Result<u16> {
    data.get(offset..offset + 2)
        .map(|s| u16::from_le_bytes([s[0], s[1]]))
        .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "读取 u16 越界"))
}

fn read_u32(data: &[u8], offset: usize) -> io::Result<u32> {
    data.get(offset..offset + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "读取 u32 越界"))
}

fn read_u64(data: &[u8], offset: usize) -> io::Result<u64> {
    data.get(offset..offset + 8)
        .map(|s| u64::from_le_bytes(s.try_into().unwrap()))
        .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "读取 u64 越界"))
}

/// 远程固件的 payload 位置（数据在远端资源中的绝对偏移）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemotePayloadSpan {
    pub data_offset: u64,
    /// 远端资源总长度。
    pub total_len: u64,
    /// payload 自身长度：裸 `CrAU` 时等于 `total_len`。
    pub payload_len: u64,
}

/// 基于项目 HTTP 客户端的远程读取器。
///
/// 直接实现 `Read + Seek`（而非只给 `RemoteRead`），这样它既满足 payload
/// 解析器，也能被别处直接使用。取消语义由调用方的进程级取消传达——
/// 读取本身是有界的（每次 Range 窗口最多 1 MiB）。
pub struct RemotePayloadReader {
    client: reqwest::blocking::Client,
    url: String,
    total_len: u64,
    /// 缓冲的当前窗口内容。
    window: Vec<u8>,
    window_start: u64,
    position: u64,
}

/// 单个 Range 窗口大小：与 `remote_firmware` 保持一致，越小则断流损失越小。
const REMOTE_WINDOW_BYTES: u64 = 1024 * 1024;

impl RemotePayloadReader {
    fn open(url: &str) -> Result<Self, String> {
        let client = reqwest::blocking::Client::builder()
            .user_agent(format!("Nwflash/{}", crate::DEFAULT_APP_VERSION))
            .connect_timeout(std::time::Duration::from_secs(20))
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .map_err(|error| error.to_string())?;
        let response = client
            .get(url)
            .header(reqwest::header::RANGE, "bytes=0-0")
            .send()
            .map_err(|error| error.to_string())?;
        let total_len = response
            .headers()
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.rsplit('/').next())
            .and_then(|value| value.trim().parse::<u64>().ok())
            .ok_or_else(|| "远程服务器未返回 Content-Range，无法按需读取。".to_string())?;
        Ok(Self {
            client,
            url: url.to_string(),
            total_len,
            window: Vec::new(),
            window_start: 0,
            position: 0,
        })
    }

    fn fill_window(&mut self, offset: u64) -> io::Result<()> {
        if offset >= self.total_len {
            self.window.clear();
            return Ok(());
        }
        let end = (offset + REMOTE_WINDOW_BYTES - 1).min(self.total_len - 1);
        let response = self
            .client
            .get(&self.url)
            .header(reqwest::header::RANGE, format!("bytes={offset}-{end}"))
            .send()
            .map_err(|error| io::Error::other(error.to_string()))?;
        // 只接受 206：服务器忽略 Range 返回 200 时，body 是**整个**固件
        // （GB 级），读进内存等于自我瘫痪，宁可立刻失败。
        if response.status() != reqwest::StatusCode::PARTIAL_CONTENT {
            return Err(io::Error::other(format!(
                "远程读取失败：HTTP {}（服务器未按 Range 返回分片）。",
                response.status()
            )));
        }
        self.window = response
            .bytes()
            .map_err(|error| io::Error::other(error.to_string()))?
            .to_vec();
        self.window_start = offset;
        Ok(())
    }
}

impl Read for RemotePayloadReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || self.position >= self.total_len {
            return Ok(0);
        }
        let in_window = self.position >= self.window_start
            && self.position < self.window_start + self.window.len() as u64;
        if !in_window {
            self.fill_window(self.position)?;
        }
        let offset_in_window = (self.position - self.window_start) as usize;
        if offset_in_window >= self.window.len() {
            return Ok(0);
        }
        let available = self.window.len() - offset_in_window;
        let count = available.min(buf.len());
        buf[..count].copy_from_slice(&self.window[offset_in_window..offset_in_window + count]);
        self.position += count as u64;
        Ok(count)
    }
}

impl Seek for RemotePayloadReader {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        let target = match position {
            SeekFrom::Start(offset) => offset as i128,
            SeekFrom::End(offset) => self.total_len as i128 + offset as i128,
            SeekFrom::Current(offset) => self.position as i128 + offset as i128,
        };
        if target < 0 || target > self.total_len as i128 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek 超出远程资源范围",
            ));
        }
        self.position = target as u64;
        Ok(self.position)
    }
}

/// 定位远程固件中的 payload，并返回可直接交给解析器的读取器。
pub fn locate_remote_payload(
    url: &str,
) -> Result<(RemotePayloadSpan, RemotePayloadReader), String> {
    let mut reader = RemotePayloadReader::open(url)?;
    let total_len = reader.total_len;

    let mut magic = [0u8; 4];
    reader
        .read_exact(&mut magic)
        .map_err(|error| error.to_string())?;

    if &magic == b"CrAU" {
        reader
            .seek(SeekFrom::Start(0))
            .map_err(|error| error.to_string())?;
        return Ok((
            RemotePayloadSpan {
                data_offset: 0,
                total_len,
                payload_len: total_len,
            },
            reader,
        ));
    }
    if &magic != b"PK\x03\x04" {
        return Err("远程固件既不是 payload 也不是 zip 包。".to_string());
    }

    let mut window = 4u64 * 1024 * 1024;
    loop {
        let effective = window.min(total_len.max(1));
        let base = total_len.saturating_sub(effective);
        let mut buffer = vec![0u8; effective as usize];
        reader
            .seek(SeekFrom::Start(base))
            .map_err(|error| error.to_string())?;
        let mut filled = 0usize;
        while filled < buffer.len() {
            let read = reader
                .read(&mut buffer[filled..])
                .map_err(|error| error.to_string())?;
            if read == 0 {
                break;
            }
            filled += read;
        }
        buffer.truncate(filled);

        match locate_payload_in_zip(&buffer, base) {
            Ok(location) => {
                let mut local_header = [0u8; 30];
                reader
                    .seek(SeekFrom::Start(location.local_header_offset))
                    .map_err(|error| error.to_string())?;
                reader
                    .read_exact(&mut local_header)
                    .map_err(|error| error.to_string())?;
                let data_offset = location
                    .data_offset(&local_header)
                    .map_err(|error| error.to_string())?;
                return Ok((
                    RemotePayloadSpan {
                        data_offset,
                        total_len,
                        payload_len: location.length,
                    },
                    reader,
                ));
            }
            Err(error) if effective >= total_len => return Err(error.to_string()),
            Err(_) => window = window.saturating_mul(2),
        }
    }
}
