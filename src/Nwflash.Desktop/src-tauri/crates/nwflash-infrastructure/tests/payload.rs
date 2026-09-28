//! payload（CrAU）解析与提取的契约测试。
//!
//! 这些测试用**合成 payload**验证解析器，不依赖任何真实固件：合成数据能精确
//! 构造边界（多 extent、空洞、各种压缩类型），而真实固件只能覆盖已经存在的那几种。

use std::io::{Seek, SeekFrom, Write};
use std::path::Path;

use nwflash_infrastructure::payload::{ExtractedPartition, Payload, PayloadError};

// ---------- 合成 payload 构造 ----------

fn varint(mut value: u64, out: &mut Vec<u8>) {
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if value == 0 {
            break;
        }
    }
}

fn tag(field: u32, wire: u32, out: &mut Vec<u8>) {
    varint(u64::from((field << 3) | wire), out);
}

fn length_delimited(field: u32, body: &[u8], out: &mut Vec<u8>) {
    tag(field, 2, out);
    varint(body.len() as u64, out);
    out.extend_from_slice(body);
}

/// payload 里一个待写入的分区描述。
struct PartitionSpec {
    name: &'static str,
    /// 每个 operation 的 (op_type, 压缩后字节)。
    operations: Vec<(u64, Vec<u8>)>,
    /// 声明的最终镜像大小。
    new_size: u64,
    /// 数据要落到的 extent 列表 (start_block, num_blocks)。
    extents: Vec<(u64, u64)>,
}

fn build_payload(block_size: u32, partitions: &[PartitionSpec]) -> Vec<u8> {
    let mut data = Vec::new();
    let mut partition_entries = Vec::new();

    for spec in partitions {
        let mut operation_entries = Vec::new();
        for (op_type, bytes) in &spec.operations {
            let data_offset = data.len() as u64;
            data.extend_from_slice(bytes);

            let mut op = Vec::new();
            tag(1, 0, &mut op);
            varint(*op_type, &mut op);
            tag(2, 0, &mut op);
            varint(data_offset, &mut op);
            tag(3, 0, &mut op);
            varint(bytes.len() as u64, &mut op);
            for (start_block, num_blocks) in &spec.extents {
                let mut extent = Vec::new();
                tag(1, 0, &mut extent);
                varint(*start_block, &mut extent);
                tag(2, 0, &mut extent);
                varint(*num_blocks, &mut extent);
                length_delimited(6, &extent, &mut op);
            }
            length_delimited(8, &op, &mut operation_entries);
        }

        let mut partition_info = Vec::new();
        tag(1, 0, &mut partition_info);
        varint(spec.new_size, &mut partition_info);

        let mut partition = Vec::new();
        length_delimited(1, spec.name.as_bytes(), &mut partition);
        length_delimited(7, &partition_info, &mut partition);
        partition.extend_from_slice(&operation_entries);
        length_delimited(13, &partition, &mut partition_entries);
    }

    let mut manifest = Vec::new();
    tag(3, 0, &mut manifest);
    varint(u64::from(block_size), &mut manifest);
    manifest.extend_from_slice(&partition_entries);

    let mut payload = Vec::new();
    payload.extend_from_slice(b"CrAU");
    payload.extend_from_slice(&2u64.to_be_bytes());
    payload.extend_from_slice(&(manifest.len() as u64).to_be_bytes());
    payload.extend_from_slice(&0u32.to_be_bytes());
    payload.extend_from_slice(&manifest);
    payload.extend_from_slice(&data);
    payload
}

fn write_payload(dir: &Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).expect("写入合成 payload");
    path
}

// ---------- 解析 ----------

#[test]
fn parses_header_manifest_and_partition_metadata() {
    let dir = tempfile::tempdir().expect("临时目录");
    let raw = vec![7u8; 4096];
    let bytes = build_payload(
        4096,
        &[PartitionSpec {
            name: "boot",
            operations: vec![(0, raw.clone())],
            new_size: 4096,
            extents: vec![(0, 1)],
        }],
    );
    let path = write_payload(dir.path(), "payload.bin", &bytes);

    let payload = Payload::parse(&path).expect("解析");
    assert_eq!(payload.manifest.block_size, 4096);
    assert_eq!(payload.manifest.partitions.len(), 1);
    let partition = &payload.manifest.partitions[0];
    assert_eq!(partition.name, "boot");
    assert_eq!(partition.new_size, 4096);
    assert_eq!(partition.operations.len(), 1);
    assert_eq!(partition.operations[0].dst_extents.len(), 1);
}

#[test]
fn rejects_a_payload_without_the_crau_magic() {
    let dir = tempfile::tempdir().expect("临时目录");
    let path = write_payload(dir.path(), "bogus.bin", &[0u8; 64]);

    let error = Payload::parse(&path).expect_err("缺少魔数必须失败");
    assert!(
        matches!(error, PayloadError::MissingMagic),
        "实际: {error:?}"
    );
}

#[test]
fn rejects_unsupported_payload_versions() {
    let dir = tempfile::tempdir().expect("临时目录");
    let mut bytes = build_payload(4096, &[]);
    bytes[4..12].copy_from_slice(&3u64.to_be_bytes());
    let path = write_payload(dir.path(), "v3.bin", &bytes);

    let error = Payload::parse(&path).expect_err("版本 3 必须失败");
    assert!(
        matches!(error, PayloadError::UnsupportedVersion(3)),
        "实际: {error:?}"
    );
}

#[test]
fn skips_unknown_manifest_fields_instead_of_failing() {
    // 未来的 Android 版本会往 manifest 里加字段；解析器必须跳过而不是报错。
    let dir = tempfile::tempdir().expect("临时目录");
    let raw = vec![1u8; 4096];
    let mut bytes = build_payload(
        4096,
        &[PartitionSpec {
            name: "boot",
            operations: vec![(0, raw)],
            new_size: 4096,
            extents: vec![(0, 1)],
        }],
    );

    // 在 manifest 末尾追加一个未知字段（field 99, varint）。
    let manifest_size = u64::from_be_bytes(bytes[12..20].try_into().unwrap()) as usize;
    let manifest_end = 24 + manifest_size;
    let mut unknown = Vec::new();
    tag(99, 0, &mut unknown);
    varint(12345, &mut unknown);
    bytes.splice(manifest_end..manifest_end, unknown.iter().copied());
    bytes[12..20].copy_from_slice(&((manifest_size + unknown.len()) as u64).to_be_bytes());

    let path = write_payload(dir.path(), "unknown-field.bin", &bytes);
    let payload = Payload::parse(&path).expect("未知字段应被跳过");
    assert_eq!(payload.manifest.partitions.len(), 1);
}

// ---------- 提取 ----------

#[test]
fn extracts_raw_operation_byte_exactly() {
    let dir = tempfile::tempdir().expect("临时目录");
    let raw: Vec<u8> = (0..4096).map(|i| (i % 251) as u8).collect();
    let bytes = build_payload(
        4096,
        &[PartitionSpec {
            name: "boot",
            operations: vec![(0, raw.clone())],
            new_size: raw.len() as u64,
            extents: vec![(0, 1)],
        }],
    );
    let path = write_payload(dir.path(), "raw.bin", &bytes);
    let out = dir.path().join("out");

    let mut payload = Payload::parse(&path).expect("解析");
    let results = payload
        .extract_partitions(&["boot"], &out, |_, _, _| {})
        .expect("提取");

    assert_eq!(results.len(), 1);
    let image = std::fs::read(out.join("boot.img")).expect("读取产物");
    assert_eq!(image, raw, "提取结果必须与原始数据逐字节一致");
}

#[test]
fn extracts_zstd_operation_byte_exactly() {
    // Android 12+ 的 OTA 用 ZSTD（op type 14）；本项目拿到的真实固件就是这种。
    let dir = tempfile::tempdir().expect("临时目录");
    let raw: Vec<u8> = (0..8192).map(|i| (i % 253) as u8).collect();
    let compressed = zstd::stream::encode_all(&raw[..], 3).expect("压缩");
    let bytes = build_payload(
        4096,
        &[PartitionSpec {
            name: "system",
            operations: vec![(14, compressed)],
            new_size: raw.len() as u64,
            extents: vec![(0, 2)],
        }],
    );
    let path = write_payload(dir.path(), "zstd.bin", &bytes);
    let out = dir.path().join("out");

    let mut payload = Payload::parse(&path).expect("解析");
    payload
        .extract_partitions(&["system"], &out, |_, _, _| {})
        .expect("提取");

    let image = std::fs::read(out.join("system.img")).expect("读取产物");
    assert_eq!(image, raw);
}

#[test]
fn extracts_xz_operation_byte_exactly() {
    let dir = tempfile::tempdir().expect("临时目录");
    let raw: Vec<u8> = (0..4096).map(|i| (i % 241) as u8).collect();
    let mut encoder = liblzma::write::XzEncoder::new(Vec::new(), 6);
    encoder.write_all(&raw).expect("压缩");
    let compressed = encoder.finish().expect("收尾");

    let bytes = build_payload(
        4096,
        &[PartitionSpec {
            name: "vendor",
            operations: vec![(8, compressed)],
            new_size: raw.len() as u64,
            extents: vec![(0, 1)],
        }],
    );
    let path = write_payload(dir.path(), "xz.bin", &bytes);
    let out = dir.path().join("out");

    let mut payload = Payload::parse(&path).expect("解析");
    payload
        .extract_partitions(&["vendor"], &out, |_, _, _| {})
        .expect("提取");

    let image = std::fs::read(out.join("vendor.img")).expect("读取产物");
    assert_eq!(image, raw);
}

#[test]
fn writes_sparse_extents_at_the_declared_offsets() {
    // 关键契约：extent 决定数据在镜像中的落点。相邻 extent 之间允许有空洞，
    // 所以不能顺序写——这个测试用「中间隔一块」的布局钉死这一点。
    let dir = tempfile::tempdir().expect("临时目录");
    let block = 4096usize;
    let first = vec![0xAAu8; block];
    let second = vec![0xBBu8; block];
    let mut combined = first.clone();
    combined.extend_from_slice(&second);

    let bytes = build_payload(
        block as u32,
        &[PartitionSpec {
            name: "boot",
            operations: vec![(0, combined)],
            new_size: (block * 3) as u64,
            // 空洞：块 0 和块 2，块 1 不写。
            extents: vec![(0, 1), (2, 1)],
        }],
    );
    let path = write_payload(dir.path(), "sparse.bin", &bytes);
    let out = dir.path().join("out");

    let mut payload = Payload::parse(&path).expect("解析");
    payload
        .extract_partitions(&["boot"], &out, |_, _, _| {})
        .expect("提取");

    let image = std::fs::read(out.join("boot.img")).expect("读取产物");
    assert_eq!(image.len(), block * 3, "镜像必须按声明大小预分配");
    assert_eq!(&image[0..block], &first[..], "第一段落在块 0");
    assert_eq!(&image[block * 2..block * 3], &second[..], "第二段落在块 2");
    assert!(
        image[block..block * 2].iter().all(|b| *b == 0),
        "未覆盖的块 1 必须是空洞（全零）"
    );
}

#[test]
fn reports_progress_that_reaches_the_declared_total() {
    let dir = tempfile::tempdir().expect("临时目录");
    let block = 4096usize;
    // 需要大于单次读缓冲（256 KiB）才会产生多次回调 —— 真实分区都是
    // 百 MB 到 GB 级，这里用 2 MiB 覆盖同样的路径。
    let blocks = 512usize;
    let raw = vec![0x5Au8; block * blocks];
    assert!(raw.len() > 256 * 1024, "测试数据必须跨过单次读缓冲");
    let bytes = build_payload(
        block as u32,
        &[PartitionSpec {
            name: "system",
            operations: vec![(0, raw.clone())],
            new_size: raw.len() as u64,
            extents: vec![(0, blocks as u64)],
        }],
    );
    let path = write_payload(dir.path(), "progress.bin", &bytes);
    let out = dir.path().join("out");

    let mut samples: Vec<(String, u64, u64)> = Vec::new();
    let mut payload = Payload::parse(&path).expect("解析");
    payload
        .extract_partitions(&["system"], &out, |name, written, total| {
            samples.push((name.to_string(), written, total));
        })
        .expect("提取");

    assert!(
        samples.len() > 1,
        "进度应当多次回调，实际 {}",
        samples.len()
    );
    assert!(
        samples.windows(2).all(|w| w[0].1 <= w[1].1),
        "进度必须单调不减: {samples:?}"
    );
    let (name, written, total) = samples.last().unwrap();
    assert_eq!(name, "system");
    assert_eq!(
        *total,
        raw.len() as u64,
        "总大小来自 manifest，调用方不必事先知道"
    );
    assert_eq!(*written, raw.len() as u64, "最后一次回调应等于总写入量");
}

#[test]
fn extracts_multiple_partitions_and_reports_each_one() {
    let dir = tempfile::tempdir().expect("临时目录");
    let block = 4096usize;
    let boot = vec![0x11u8; block];
    let vendor = vec![0x22u8; block * 2];

    let bytes = build_payload(
        block as u32,
        &[
            PartitionSpec {
                name: "boot",
                operations: vec![(0, boot.clone())],
                new_size: boot.len() as u64,
                extents: vec![(0, 1)],
            },
            PartitionSpec {
                name: "vendor",
                operations: vec![(0, vendor.clone())],
                new_size: vendor.len() as u64,
                extents: vec![(0, 2)],
            },
        ],
    );
    let path = write_payload(dir.path(), "multi.bin", &bytes);
    let out = dir.path().join("out");

    let mut seen: Vec<String> = Vec::new();
    let mut payload = Payload::parse(&path).expect("解析");
    let results = payload
        .extract_partitions(&["boot", "vendor"], &out, |name, _, _| {
            if seen.last().map(String::as_str) != Some(name) {
                seen.push(name.to_string());
            }
        })
        .expect("提取");

    assert_eq!(results.len(), 2);
    let names: Vec<&str> = results.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, vec!["boot", "vendor"]);
    assert_eq!(seen, vec!["boot", "vendor"], "每个分区都要有进度回调");
    assert_eq!(std::fs::read(out.join("boot.img")).unwrap(), boot);
    assert_eq!(std::fs::read(out.join("vendor.img")).unwrap(), vendor);
}

#[test]
fn rejects_a_partition_that_is_not_in_the_manifest() {
    let dir = tempfile::tempdir().expect("临时目录");
    let bytes = build_payload(
        4096,
        &[PartitionSpec {
            name: "boot",
            operations: vec![(0, vec![0u8; 4096])],
            new_size: 4096,
            extents: vec![(0, 1)],
        }],
    );
    let path = write_payload(dir.path(), "missing.bin", &bytes);

    let mut payload = Payload::parse(&path).expect("解析");
    let error = payload
        .extract_partitions(&["nosuch"], &dir.path().join("out"), |_, _, _| {})
        .expect_err("不存在的分区必须失败");
    assert!(
        matches!(error, PayloadError::MissingPartition(ref name) if name == "nosuch"),
        "实际: {error:?}"
    );
}

#[test]
fn rejects_an_unsupported_operation_type() {
    let dir = tempfile::tempdir().expect("临时目录");
    let bytes = build_payload(
        4096,
        &[PartitionSpec {
            name: "boot",
            // 10 = BROTLI_BSDIFF，差分操作，明确不支持。
            operations: vec![(10, vec![0u8; 128])],
            new_size: 4096,
            extents: vec![(0, 1)],
        }],
    );
    let path = write_payload(dir.path(), "unsupported.bin", &bytes);

    let mut payload = Payload::parse(&path).expect("解析");
    let error = payload
        .extract_partitions(&["boot"], &dir.path().join("out"), |_, _, _| {})
        .expect_err("不支持的 op 必须失败");
    assert!(
        matches!(error, PayloadError::UnsupportedOperation(10)),
        "实际: {error:?}"
    );
}

#[test]
fn from_reader_handles_a_payload_that_does_not_start_at_zero() {
    // 真实场景：payload.bin 嵌在 zip 里，CrAU 不在源的开头。
    let dir = tempfile::tempdir().expect("临时目录");
    let raw = vec![0x33u8; 4096];
    let mut bytes = vec![0xFFu8; 61]; // 模拟 zip 本地头占位
    bytes.extend_from_slice(&build_payload(
        4096,
        &[PartitionSpec {
            name: "boot",
            operations: vec![(0, raw.clone())],
            new_size: raw.len() as u64,
            extents: vec![(0, 1)],
        }],
    ));
    let path = write_payload(dir.path(), "wrapped.bin", &bytes);
    let out = dir.path().join("out");

    let mut cursor = std::fs::File::open(&path).expect("打开");
    let total = cursor.metadata().unwrap().len();
    cursor.seek(SeekFrom::Start(0)).unwrap();
    let mut payload = Payload::from_reader_at(&mut cursor, total, 61).expect("带基准解析");
    payload
        .extract_partitions(&["boot"], &out, |_, _, _| {})
        .expect("提取");

    assert_eq!(std::fs::read(out.join("boot.img")).unwrap(), raw);
}

/// 契约：`ExtractedPartition` 必须如实报告写入量与声明总量。
#[test]
fn extracted_partition_reports_written_and_declared_sizes() {
    let dir = tempfile::tempdir().expect("临时目录");
    let raw = vec![0x44u8; 8192];
    let bytes = build_payload(
        4096,
        &[PartitionSpec {
            name: "boot",
            operations: vec![(0, raw.clone())],
            new_size: raw.len() as u64,
            extents: vec![(0, 2)],
        }],
    );
    let path = write_payload(dir.path(), "sizes.bin", &bytes);

    let mut payload = Payload::parse(&path).expect("解析");
    let results = payload
        .extract_partitions(&["boot"], &dir.path().join("out"), |_, _, _| {})
        .expect("提取");

    let ExtractedPartition {
        name,
        bytes_written,
        total_bytes,
        ..
    } = &results[0];
    assert_eq!(name, "boot");
    assert_eq!(*total_bytes, raw.len() as u64);
    assert_eq!(*bytes_written, raw.len() as u64);
}

// ---------- 取消与健壮性 ----------

#[test]
fn cancellation_stops_extraction_midway() {
    // 提取可能持续数分钟（远程解压数 GB），取消必须在拷贝循环内部生效，
    // 而不是等整个 operation 甚至整个分区跑完。
    let dir = tempfile::tempdir().expect("临时目录");
    let raw = vec![0x11u8; 4096];
    let bytes = build_payload(
        4096,
        &[PartitionSpec {
            name: "boot",
            operations: vec![(0, raw)],
            new_size: 4096,
            extents: vec![(0, 1)],
        }],
    );
    let path = write_payload(dir.path(), "cancel.bin", &bytes);
    let out = dir.path().join("out");

    let mut payload = Payload::parse(&path).expect("解析");
    let mut checks = 0usize;
    let error = payload
        .extract_partitions_with_cancel(
            &["boot"],
            &out,
            |_, _, _| {},
            || {
                checks += 1;
                // 第 1 次检查放行（读入数据块），第 2 次取消。
                checks > 1
            },
        )
        .expect_err("取消必须中断提取");
    assert!(matches!(error, PayloadError::Canceled), "实际: {error:?}");
}

#[test]
fn rejects_a_manifest_size_beyond_the_source_length() {
    // manifest 大小来自文件头（内容可控）：越过源末尾的声明必须在分配
    // 内存**之前**被拒绝，否则损坏文件的一个头字段就是一次进程级 abort。
    let dir = tempfile::tempdir().expect("临时目录");
    let mut bytes = build_payload(4096, &[]);
    bytes[12..20].copy_from_slice(&(1u64 << 40).to_be_bytes());
    let path = write_payload(dir.path(), "huge-manifest.bin", &bytes);

    let error = Payload::parse(&path).expect_err("越界 manifest 必须失败");
    assert!(matches!(error, PayloadError::Corrupt(_)), "实际: {error:?}");
}

#[test]
fn rejects_partition_names_with_path_semantics() {
    // 分区名来自 manifest（文件内容可控），带路径语义的名字拼进 `{name}.img`
    // 会把写盘位置移出输出目录。
    let dir = tempfile::tempdir().expect("临时目录");
    let raw = vec![0x22u8; 4096];
    let bytes = build_payload(
        4096,
        &[PartitionSpec {
            name: "../evil",
            operations: vec![(0, raw)],
            new_size: 4096,
            extents: vec![(0, 1)],
        }],
    );
    let path = write_payload(dir.path(), "traversal.bin", &bytes);
    let out = dir.path().join("out");

    let mut payload = Payload::parse(&path).expect("解析");
    let error = payload
        .extract_partitions(&["../evil"], &out, |_, _, _| {})
        .expect_err("路径语义分区名必须被拒绝");
    assert!(matches!(error, PayloadError::Corrupt(_)), "实际: {error:?}");
}

#[test]
fn zero_operation_progresses_by_extent_total() {
    // AOSP 的 ZERO 操作没有数据负载（data_length=0），输出长度由 dst_extents
    // 决定。此前按 data_length 计零（=0），进度与 bytes_written 都少计。
    let dir = tempfile::tempdir().expect("临时目录");
    let bytes = build_payload(
        4096,
        &[PartitionSpec {
            name: "boot",
            operations: vec![(6, Vec::new())],
            new_size: 4096,
            extents: vec![(0, 1)],
        }],
    );
    let path = write_payload(dir.path(), "zero.bin", &bytes);
    let out = dir.path().join("out");

    let mut payload = Payload::parse(&path).expect("解析");
    let mut seen = Vec::new();
    let results = payload
        .extract_partitions(&["boot"], &out, |_, written, _| seen.push(written))
        .expect("提取");

    assert_eq!(results[0].bytes_written, 4096, "ZERO 必须按 extents 总量计");
    assert_eq!(seen.last(), Some(&4096), "进度必须走到满");
    assert_eq!(
        std::fs::read(&results[0].output_path).expect("读取产物"),
        vec![0u8; 4096],
        "extent 覆盖区必须全零"
    );
}
