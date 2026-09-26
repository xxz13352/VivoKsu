//! 远程 payload 定位的契约测试。
//!
//! 用**合成的 ZIP**（含 ZIP64 占位符与 extra 字段）验证定位逻辑，不依赖网络。
//! 真实固件的 ZIP64 形态已被这些用例覆盖：`payload.bin` 的 size 在中央目录里
//! 是 `0xFFFFFFFF`，真值只在 ZIP64 extra 里。

use std::io::Write;

use nwflash_infrastructure::payload::remote::{locate_payload_in_zip, RemoteRead, RemoteReader};
use std::io::{Read, Seek, SeekFrom};

/// 构造一个最小 ZIP：单个 `payload.bin` 成员，可选 ZIP64 extra。
fn build_zip(payload: &[u8], zip64: bool, include_local_header: bool) -> Vec<u8> {
    let name = b"payload.bin";
    let mut out = Vec::new();

    let local_header_offset = 0u64;
    if include_local_header {
        // 本地头：固定 30 字节 + 名字 + extra。
        out.extend_from_slice(b"PK\x03\x04");
        out.extend_from_slice(&45u16.to_le_bytes()); // version needed
        out.extend_from_slice(&0u16.to_le_bytes()); // flags
        out.extend_from_slice(&0u16.to_le_bytes()); // method = store
        out.extend_from_slice(&0u16.to_le_bytes()); // time
        out.extend_from_slice(&0u16.to_le_bytes()); // date
        out.extend_from_slice(&0u32.to_le_bytes()); // crc32
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes()); // csize
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes()); // usize
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&8u16.to_le_bytes()); // extra len
        out.extend_from_slice(name);
        out.extend_from_slice(&[0u8; 8]); // extra 占位
        out.extend_from_slice(payload);
    }

    // 中央目录
    let central_offset = out.len() as u64;
    out.extend_from_slice(b"PK\x01\x02");
    out.extend_from_slice(&45u16.to_le_bytes()); // version made by
    out.extend_from_slice(&45u16.to_le_bytes()); // version needed
    out.extend_from_slice(&0u16.to_le_bytes()); // flags
    out.extend_from_slice(&0u16.to_le_bytes()); // method
    out.extend_from_slice(&0u16.to_le_bytes()); // time
    out.extend_from_slice(&0u16.to_le_bytes()); // date
    out.extend_from_slice(&0u32.to_le_bytes()); // crc32
    if zip64 {
        out.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes()); // csize 占位
        out.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes()); // usize 占位
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes()); // extra len
        out.extend_from_slice(&0u16.to_le_bytes()); // comment len
        out.extend_from_slice(&0u16.to_le_bytes()); // disk start
        out.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
        out.extend_from_slice(&0u32.to_le_bytes()); // external attrs
        out.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes()); // lho 占位
    } else {
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // extra len
        out.extend_from_slice(&0u16.to_le_bytes()); // comment len
        out.extend_from_slice(&0u16.to_le_bytes()); // disk start
        out.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
        out.extend_from_slice(&0u32.to_le_bytes()); // external attrs
        out.extend_from_slice(&(local_header_offset as u32).to_le_bytes());
    }
    out.extend_from_slice(name);

    if zip64 {
        // ZIP64 extra: id=1, size=16, usize, csize（lho 占位符之外的真值顺序）
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(&(payload.len() as u64).to_le_bytes()); // usize
        out.extend_from_slice(&(payload.len() as u64).to_le_bytes()); // csize
    }

    let central_size = out.len() as u64 - central_offset;

    if zip64 {
        let zip64_eocd_offset = out.len() as u64;
        out.extend_from_slice(b"PK\x06\x06");
        out.extend_from_slice(&44u64.to_le_bytes()); // size of record
        out.extend_from_slice(&45u16.to_le_bytes());
        out.extend_from_slice(&45u16.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&1u64.to_le_bytes()); // entries on disk
        out.extend_from_slice(&1u64.to_le_bytes()); // total entries
        out.extend_from_slice(&central_size.to_le_bytes());
        out.extend_from_slice(&central_offset.to_le_bytes());
        let _ = zip64_eocd_offset;
    }

    // EOCD
    out.extend_from_slice(b"PK\x05\x06");
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&(central_size as u32).to_le_bytes());
    out.extend_from_slice(&(central_offset as u32).to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

#[test]
fn locates_payload_bin_in_a_plain_zip() {
    let payload = b"CrAU-test-payload-bytes";
    let zip = build_zip(payload, false, true);

    let location = locate_payload_in_zip(&zip, 0).expect("定位");
    assert_eq!(location.length, payload.len() as u64);
    // 数据偏移一律经 `data_offset` 现算：本地头的 extra 长度只能从那 30 字节里
    // 读，而本地头常常不在「文件尾部窗口」内。
    let offset = location
        .data_offset(&zip[location.local_header_offset as usize..])
        .expect("算偏移");
    // 本地头 30 + 名字 11 + extra 8 = 49
    assert_eq!(offset, 49, "数据偏移必须跳过本地头与 extra");
}

#[test]
fn reads_the_true_size_from_the_zip64_extra_field() {
    // 真实固件（9.3 GB）就是这种形态：中央目录里的 size 是 0xFFFFFFFF，
    // 只有 ZIP64 extra 里才是真值。读占位符会把长度算成 4 GB，后续解析全错。
    let large = vec![0x41u8; 4096];
    let zip = build_zip(&large, true, true);

    let location = locate_payload_in_zip(&zip, 0).expect("定位");
    assert_eq!(
        location.length,
        large.len() as u64,
        "必须用 ZIP64 extra 里的真值，而不是 0xFFFFFFFF 占位符"
    );
}

#[test]
fn skips_the_local_header_extra_when_computing_the_data_offset() {
    // 本地头的 extra 长度只能从本地头本身读；用中央目录那份会算错偏移。
    // 这里构造本地头 extra 长度与中央目录 extra 长度**不同**的 zip 来钉死这点。
    let zip = build_zip(b"CrAU", false, true);

    let location = locate_payload_in_zip(&zip, 0).expect("定位");
    let local_header = &zip[location.local_header_offset as usize..];
    assert_eq!(&local_header[0..4], b"PK\x03\x04", "本地头签名");

    let offset = location.data_offset(local_header).expect("算偏移");
    // 本地头 extra 是 8 字节（不是中央目录的 0），所以偏移必须包含它。
    let local_extra = u16::from_le_bytes([local_header[28], local_header[29]]);
    assert_eq!(local_extra, 8, "本地头 extra 长度");
    assert_eq!(offset, 30 + 11 + 8, "偏移必须用本地头的 extra 长度");
}

#[test]
fn reports_missing_payload_bin_rather_than_guessing() {
    // 只含一个无关成员的 zip。
    let mut zip = Vec::new();
    let name = b"other.bin";
    zip.extend_from_slice(b"PK\x01\x02");
    zip.extend_from_slice(&[0u8; 24]);
    zip.extend_from_slice(&(name.len() as u16).to_le_bytes());
    zip.extend_from_slice(&[0u8; 8]);
    zip.extend_from_slice(&[0u8; 4]);
    zip.extend_from_slice(&[0u8; 4]);
    zip.extend_from_slice(name);
    let central_offset = 0u32;
    let central_size = zip.len() as u32;
    zip.extend_from_slice(b"PK\x05\x06");
    zip.extend_from_slice(&[0u8; 4]);
    zip.extend_from_slice(&1u16.to_le_bytes());
    zip.extend_from_slice(&1u16.to_le_bytes());
    zip.extend_from_slice(&central_size.to_le_bytes());
    zip.extend_from_slice(&central_offset.to_le_bytes());
    zip.extend_from_slice(&[0u8; 2]);

    let error = locate_payload_in_zip(&zip, 0).expect_err("必须报错");
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
}

#[test]
fn rejects_a_buffer_that_does_not_cover_the_central_directory() {
    // 尾部窗口没抓全时，不能拿半个中央目录去解析，必须明确报错。
    let zip = build_zip(b"CrAU", false, true);
    let truncated = &zip[..zip.len() / 2];

    assert!(locate_payload_in_zip(truncated, 0).is_err());
}

// ---------- RemoteReader 适配 ----------

/// 内存实现的远程源，用来验证 `RemoteReader` 的 Read/Seek 语义。
struct MemoryRemote {
    data: Vec<u8>,
}

impl RemoteRead for MemoryRemote {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
        if offset >= self.data.len() as u64 {
            return Ok(0);
        }
        let start = offset as usize;
        let count = buf.len().min(self.data.len() - start);
        buf[..count].copy_from_slice(&self.data[start..start + count]);
        Ok(count)
    }

    fn total_len(&self) -> u64 {
        self.data.len() as u64
    }
}

#[test]
fn remote_reader_supports_seek_and_read() {
    let data: Vec<u8> = (0..=255u8).collect();
    let mut reader = RemoteReader::new(MemoryRemote { data: data.clone() });

    let mut head = [0u8; 4];
    reader.read_exact(&mut head).expect("读头部");
    assert_eq!(head, [0, 1, 2, 3]);

    reader.seek(SeekFrom::Start(100)).expect("seek");
    let mut mid = [0u8; 3];
    reader.read_exact(&mut mid).expect("读中段");
    assert_eq!(mid, [100, 101, 102]);

    // SeekFrom::End 在远程源上必须也成立（解析器读尾部时用到）。
    reader.seek(SeekFrom::End(-2)).expect("seek from end");
    let mut tail = [0u8; 2];
    reader.read_exact(&mut tail).expect("读尾部");
    assert_eq!(tail, [254, 255]);

    // 读到末尾之后再读应得到 EOF 而不是错误。
    reader.seek(SeekFrom::Start(data.len() as u64)).unwrap();
    let mut eof = [0u8; 1];
    assert_eq!(reader.read(&mut eof).expect("EOF"), 0);
}

#[test]
fn remote_reader_rejects_negative_seek() {
    let mut reader = RemoteReader::new(MemoryRemote { data: vec![0u8; 8] });
    let error = reader
        .seek(SeekFrom::Current(-1))
        .expect_err("负偏移必须失败");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
}

/// 端到端：从「远程」源解析一个 zip 内嵌的 payload 并提取。
#[test]
fn parses_and_extracts_from_a_remote_reader() {
    use nwflash_infrastructure::payload::Payload;

    // 复用 payload 测试里的合成构造思路：这里只需要一个能解析的最小 payload。
    let mut payload_bytes = Vec::new();
    payload_bytes.extend_from_slice(b"CrAU");
    payload_bytes.extend_from_slice(&2u64.to_be_bytes());
    payload_bytes.extend_from_slice(&0u64.to_be_bytes()); // 空 manifest
    payload_bytes.extend_from_slice(&0u32.to_be_bytes());

    let zip = build_zip(&payload_bytes, false, true);
    let location = locate_payload_in_zip(&zip, 0).expect("定位");
    let data_offset = location
        .data_offset(&zip[location.local_header_offset as usize..])
        .expect("偏移");

    let reader = RemoteReader::new(MemoryRemote { data: zip });
    let parsed = Payload::from_reader_at(reader, location.length, data_offset).expect("解析");
    // 空 manifest 也应能解析出 0 个分区，而不是报错。
    assert_eq!(parsed.manifest.partitions.len(), 0);

    let _ = std::io::sink().write_all(&[]);
}
