using System.Buffers.Binary;
using System.IO;
using System.IO.Compression;
using System.Net.Http;
using System.Text;

namespace VivoKsu.App.Services;

/// <summary>远程 ZIP 的一个成员(中央目录解析结果)。</summary>
/// <param name="FullName">ZIP 内完整路径。</param>
/// <param name="Name">去目录、去扩展名后的分区名(如 "boot")。</param>
/// <param name="SizeBytes">解压后大小。</param>
/// <param name="Crc32">中央目录记录的 CRC-32(解压后必须校验)。</param>
/// <param name="CompressionMethod">0 = store,8 = deflate。</param>
public sealed record RemoteZipMember(string FullName, string Name, long SizeBytes, uint Crc32, ushort CompressionMethod);

/// <summary>一次成员提取的结果。</summary>
/// <param name="PartitionName">分区名(去扩展名的成员名)。</param>
/// <param name="Path">提取出的本地文件路径。</param>
/// <param name="SizeBytes">解压后字节数。</param>
public sealed record ExtractedRemoteImage(string PartitionName, string Path, long SizeBytes);

/// <summary>
/// 远程 ZIP 流式读取器(对齐 Rust <c>remote_firmware.rs</c>):
/// 探测格式 / 只拉中央目录列成员 / 按需解压目标成员 —— 全程只下载用到的字节。
/// <para>
/// <b>刻意不用</b> .NET <see cref="ZipArchive"/>:实测其 Read 模式<b>不校验 CRC</b>
/// (损坏数据静默解压成功),对刷机工具不可接受。这里自行解析 ZIP 结构
/// (含 zip64 大文件/大偏移),解压后显式校验 CRC-32 与长度,与 Rust zip crate 行为一致。
/// </para>
/// </summary>
public static class RemoteZipClient
{
    private const uint EocdSignature = 0x0605_4B50;
    private const uint Zip64LocatorSignature = 0x0706_4B50;
    private const uint Zip64EocdSignature = 0x0606_4B50;
    private const uint CentralHeaderSignature = 0x0201_4B50;
    private const uint LocalHeaderSignature = 0x0403_4B50;

    private const ushort MethodStore = 0;
    private const ushort MethodDeflate = 8;

    /// <summary>探测远程固件类型:payload 原始镜像("CrAU" 魔数)/ ZIP / 不支持。</summary>
    public static async Task<RemoteFirmwareKind> ProbeKindAsync(HttpClient http, string url, CancellationToken cancellationToken = default)
    {
        RemoteRangeStream.ValidateUrl(url);
        var head = await FetchAsync(http, url, 0, 3, cancellationToken).ConfigureAwait(false);
        if (head.Length >= 4 && head[0] == (byte)'C' && head[1] == (byte)'r' && head[2] == (byte)'A' && head[3] == (byte)'U')
        {
            return RemoteFirmwareKind.PayloadRaw;
        }

        if (head.Length >= 2 && head[0] == (byte)'P' && head[1] == (byte)'K')
        {
            var members = await ListMembersAsync(http, url, cancellationToken).ConfigureAwait(false);
            return members.Any(member => member.FullName == "payload.bin")
                ? RemoteFirmwareKind.PayloadZip
                : RemoteFirmwareKind.DirectImageZip;
        }

        return RemoteFirmwareKind.Unsupported;
    }

    /// <summary>列出远程 ZIP 全部文件成员(只拉中央目录;目录条目跳过)。</summary>
    public static async Task<IReadOnlyList<RemoteZipMember>> ListMembersAsync(HttpClient http, string url, CancellationToken cancellationToken = default)
    {
        RemoteRangeStream.ValidateUrl(url);
        return await Task.Run(async () =>
        {
            var totalLength = await ProbeLengthAsync(http, url, cancellationToken).ConfigureAwait(false);
            var central = await ReadCentralDirectoryAsync(http, url, totalLength, cancellationToken).ConfigureAwait(false);
            var members = new List<RemoteZipMember>(central.Count);
            foreach (var entry in central)
            {
                if (entry.FullName.EndsWith("/", StringComparison.Ordinal))
                {
                    continue;
                }

                members.Add(new RemoteZipMember(
                    entry.FullName,
                    PartitionNameOf(entry.FullName),
                    entry.UncompressedSize,
                    entry.Crc32,
                    entry.Method));
            }

            return (IReadOnlyList<RemoteZipMember>)members;
        }, cancellationToken).ConfigureAwait(false);
    }

    /// <summary>
    /// 从远程 ZIP 按需解压目标分区成员到 <paramref name="outputDir"/>。
    /// 同一分区多个候选时取 ZIP 中第一个;写到 <c>.partial</c> 临时文件,
    /// 解压完成后<b>显式校验 CRC-32 与长度</b>再原子改名 —— 中断/损坏都不会留下可误用的半截 .img。
    /// </summary>
    /// <param name="wantedPartitionNames">目标分区名(去扩展名匹配,如 ["init_boot","boot","vendor_boot"])。</param>
    /// <param name="reportProgress">每解压完一个成员回调 (分区名, 解压字节数)。</param>
    public static async Task<IReadOnlyList<ExtractedRemoteImage>> ExtractMembersAsync(
        HttpClient http,
        string url,
        IReadOnlyCollection<string> wantedPartitionNames,
        string outputDir,
        Action<string, long>? reportProgress = null,
        CancellationToken cancellationToken = default)
    {
        RemoteRangeStream.ValidateUrl(url);
        ArgumentException.ThrowIfNullOrWhiteSpace(outputDir);
        if (wantedPartitionNames.Count == 0)
        {
            throw new ArgumentException("至少指定一个目标分区。", nameof(wantedPartitionNames));
        }

        Directory.CreateDirectory(outputDir);

        return await Task.Run(async () =>
        {
            var totalLength = await ProbeLengthAsync(http, url, cancellationToken).ConfigureAwait(false);
            var central = await ReadCentralDirectoryAsync(http, url, totalLength, cancellationToken).ConfigureAwait(false);
            var results = new List<ExtractedRemoteImage>();
            foreach (var wanted in wantedPartitionNames)
            {
                cancellationToken.ThrowIfCancellationRequested();
                var entry = central.FirstOrDefault(candidate =>
                    !candidate.FullName.EndsWith("/", StringComparison.Ordinal)
                    && PartitionNameOf(candidate.FullName) == wanted);
                if (entry is null)
                {
                    continue;
                }

                results.Add(await ExtractSingleAsync(http, url, entry, wanted, outputDir, reportProgress, cancellationToken).ConfigureAwait(false));
            }

            return (IReadOnlyList<ExtractedRemoteImage>)results;
        }, cancellationToken).ConfigureAwait(false);
    }

    private static async Task<ExtractedRemoteImage> ExtractSingleAsync(
        HttpClient http,
        string url,
        CentralEntry entry,
        string partitionName,
        string outputDir,
        Action<string, long>? reportProgress,
        CancellationToken cancellationToken)
    {
        // local file header:签名(4) 版本(2) flags(2) method(2) 时间(2) 日期(2) crc(4)
        // compSize(4) uncompSize(4) nameLen(2) extraLen(2) → 固定 30 字节,数据区紧跟名字+额外字段。
        var localHeader = await FetchAsync(http, url, entry.LocalHeaderOffset, entry.LocalHeaderOffset + 29, cancellationToken).ConfigureAwait(false);
        var localSignature = BinaryPrimitives.ReadUInt32LittleEndian(localHeader);
        if (localSignature != LocalHeaderSignature)
        {
            throw new IOException($"ZIP 数据区签名不合法({localSignature:X8})。");
        }

        var nameLength = BinaryPrimitives.ReadUInt16LittleEndian(localHeader.AsSpan(26));
        var extraLength = BinaryPrimitives.ReadUInt16LittleEndian(localHeader.AsSpan(28));
        var dataStart = entry.LocalHeaderOffset + 30 + nameLength + extraLength;

        var outputPath = Path.Combine(outputDir, $"{partitionName}.img");
        var partialPath = Path.Combine(outputDir, $".{partitionName}.partial");
        var crc = Crc32.Create();

        try
        {
            await using (var target = new FileStream(partialPath, FileMode.Create, FileAccess.Write, FileShare.None))
            {
                if (entry.Method == MethodStore)
                {
                    await CopyRangeAsync(http, url, dataStart, dataStart + entry.CompressedSize - 1, target, reportProgress, partitionName, cancellationToken).ConfigureAwait(false);
                }
                else if (entry.Method == MethodDeflate)
                {
                    // ZIP 的 deflate 是裸 deflate(无 zlib 头),BCL DeflateStream 恰好匹配。
                    await using var compressed = new SubRangeStream(http, url, dataStart, entry.CompressedSize, cancellationToken, reportProgress, partitionName);
                    await using var deflate = new DeflateStream(compressed, CompressionMode.Decompress);
                    var buffer = new byte[256 * 1024];
                    long written = 0;
                    int read;
                    while ((read = await deflate.ReadAsync(buffer, cancellationToken).ConfigureAwait(false)) > 0)
                    {
                        await target.WriteAsync(buffer.AsMemory(0, read), cancellationToken).ConfigureAwait(false);
                        crc.Append(buffer.AsSpan(0, read));
                        written += read;
                        reportProgress?.Invoke(partitionName, written);
                    }
                }
                else
                {
                    throw new IOException($"ZIP 成员 {entry.FullName} 使用了不支持的压缩方式({entry.Method})。");
                }
            }

            if (crc.GetCurrentHashAsUInt32() != entry.Crc32)
            {
                throw new IOException($"ZIP 成员 {entry.FullName} 校验失败(CRC 不匹配,数据已损坏)。");
            }

            if (entry.UncompressedSize != new FileInfo(partialPath).Length)
            {
                throw new IOException($"ZIP 成员 {entry.FullName} 长度不符(期望 {entry.UncompressedSize} 字节)。");
            }

            File.Move(partialPath, outputPath, overwrite: true);
            return new ExtractedRemoteImage(partitionName, outputPath, entry.UncompressedSize);
        }
        finally
        {
            TryDelete(partialPath);
        }
    }

    // ---------------- ZIP 结构解析(EOCD / zip64 / 中央目录) ----------------

    private sealed record CentralEntry(string FullName, long UncompressedSize, long CompressedSize, uint Crc32, ushort Method, long LocalHeaderOffset);

    /// <summary>Range(0,0) 探测远程文件总长(206 的 Content-Range 尾段)。</summary>
    private static async Task<long> ProbeLengthAsync(HttpClient http, string url, CancellationToken cancellationToken)
    {
        using var request = new HttpRequestMessage(HttpMethod.Get, url);
        request.Headers.Range = new System.Net.Http.Headers.RangeHeaderValue(0, 0);
        using var response = await http.SendAsync(request, HttpCompletionOption.ResponseHeadersRead, cancellationToken).ConfigureAwait(false);
        if (response.StatusCode != System.Net.HttpStatusCode.PartialContent)
        {
            throw new IOException("固件服务器不支持分段下载(未返回 206),请改用手动选择镜像或整包下载。");
        }

        var length = response.Content.Headers.ContentRange?.Length;
        return length is > 0
            ? length.Value
            : throw new IOException("固件服务器未返回文件大小(Content-Range 缺失)。");
    }

    private static async Task<byte[]> FetchAsync(HttpClient http, string url, long start, long endInclusive, CancellationToken cancellationToken)
    {
        using var request = new HttpRequestMessage(HttpMethod.Get, url);
        request.Headers.Range = new System.Net.Http.Headers.RangeHeaderValue(start, endInclusive);
        using var response = await http.SendAsync(request, HttpCompletionOption.ResponseHeadersRead, cancellationToken).ConfigureAwait(false);
        response.EnsureSuccessStatusCode();
        if (response.StatusCode != System.Net.HttpStatusCode.PartialContent)
        {
            throw new IOException("固件服务器不支持分段下载(未返回 206)。");
        }

        var expected = (int)(endInclusive - start + 1);
        await using var stream = await response.Content.ReadAsStreamAsync(cancellationToken).ConfigureAwait(false);
        using var output = new MemoryStream(expected);
        await stream.CopyToAsync(output, 64 * 1024, cancellationToken).ConfigureAwait(false);
        var bytes = output.ToArray();
        if (bytes.Length != expected)
        {
            throw new IOException($"远程读取返回长度不符:期望 {expected} 字节,实际 {bytes.Length} 字节。");
        }

        return bytes;
    }

    private static async Task CopyRangeAsync(
        HttpClient http, string url, long start, long endInclusive, Stream target,
        Action<string, long>? reportProgress, string partitionName, CancellationToken cancellationToken)
    {
        await using var source = new SubRangeStream(http, url, start, endInclusive - start + 1, cancellationToken, reportProgress, partitionName);
        await source.CopyToAsync(target, 256 * 1024, cancellationToken).ConfigureAwait(false);
    }

    private static async Task<List<CentralEntry>> ReadCentralDirectoryAsync(HttpClient http, string url, long totalLength, CancellationToken cancellationToken)
    {
        // 读尾部找 EOCD(EOCD 最长 65557 字节)。
        var tailLength = Math.Min(totalLength, 65557);
        var tail = await FetchAsync(http, url, totalLength - tailLength, totalLength - 1, cancellationToken).ConfigureAwait(false);
        var eocdIndex = FindLastSignature(tail, EocdSignature);
        if (eocdIndex < 0)
        {
            throw new IOException("不是有效的 ZIP 固件包(未找到中央目录)。");
        }

        var entryCount = (int)BinaryPrimitives.ReadUInt16LittleEndian(tail.AsSpan(eocdIndex + 10));
        long cdSize = BinaryPrimitives.ReadUInt32LittleEndian(tail.AsSpan(eocdIndex + 12));
        long cdOffset = BinaryPrimitives.ReadUInt32LittleEndian(tail.AsSpan(eocdIndex + 16));

        // zip64:EOCD 前有 locator,真实 CD 位置/条目数在 zip64 EOCD 里。
        if ((cdOffset == 0xFFFF_FFFF || cdSize == 0xFFFF_FFFF || entryCount == 0xFFFF)
            && eocdIndex >= 20
            && BinaryPrimitives.ReadUInt32LittleEndian(tail.AsSpan(eocdIndex - 20)) == Zip64LocatorSignature)
        {
            var zip64EocdOffset = BinaryPrimitives.ReadInt64LittleEndian(tail.AsSpan(eocdIndex - 12));
            var zip64Eocd = await FetchAsync(http, url, zip64EocdOffset, zip64EocdOffset + 55, cancellationToken).ConfigureAwait(false);
            if (BinaryPrimitives.ReadUInt32LittleEndian(zip64Eocd) != Zip64EocdSignature)
            {
                throw new IOException("ZIP64 中央目录结构不合法。");
            }

            entryCount = checked((int)BinaryPrimitives.ReadInt64LittleEndian(zip64Eocd.AsSpan(32)));
            cdSize = BinaryPrimitives.ReadInt64LittleEndian(zip64Eocd.AsSpan(40));
            cdOffset = BinaryPrimitives.ReadInt64LittleEndian(zip64Eocd.AsSpan(48));
        }

        var central = await FetchAsync(http, url, cdOffset, cdOffset + cdSize - 1, cancellationToken).ConfigureAwait(false);
        var entries = new List<CentralEntry>(entryCount);
        var cursor = 0;
        for (var index = 0; index < entryCount; index++)
        {
            // 同步解析单条目(C# 12 async 方法内禁用 Span 局部变量,解析逻辑全部抽到同步方法)。
            var (entry, consumed) = ParseCentralEntry(central, cursor);
            if (consumed == 0)
            {
                throw new IOException("ZIP 中央目录条目不合法。");
            }

            entries.Add(entry);
            cursor += consumed;
        }

        return entries;
    }

    /// <summary>解析中央目录单条目(纯同步,含 zip64 extra field)。返回 (条目, 消耗字节数;0 = 结构非法)。</summary>
    private static (CentralEntry Entry, int Consumed) ParseCentralEntry(byte[] central, int cursor)
    {
        if (cursor + 46 > central.Length || BinaryPrimitives.ReadUInt32LittleEndian(central.AsSpan(cursor)) != CentralHeaderSignature)
        {
            return (new CentralEntry(string.Empty, 0, 0, 0, 0, 0), 0);
        }

        var method = BinaryPrimitives.ReadUInt16LittleEndian(central.AsSpan(cursor + 10));
        var crc32 = BinaryPrimitives.ReadUInt32LittleEndian(central.AsSpan(cursor + 16));
        long compressedSize = BinaryPrimitives.ReadUInt32LittleEndian(central.AsSpan(cursor + 20));
        long uncompressedSize = BinaryPrimitives.ReadUInt32LittleEndian(central.AsSpan(cursor + 24));
        var nameLength = BinaryPrimitives.ReadUInt16LittleEndian(central.AsSpan(cursor + 28));
        var extraLength = BinaryPrimitives.ReadUInt16LittleEndian(central.AsSpan(cursor + 30));
        var commentLength = BinaryPrimitives.ReadUInt16LittleEndian(central.AsSpan(cursor + 32));
        long localOffset = BinaryPrimitives.ReadUInt32LittleEndian(central.AsSpan(cursor + 42));
        var fullName = Encoding.UTF8.GetString(central, cursor + 46, nameLength);

        // zip64:真实值在 extra field 的 0x0001 头里,顺序为 uncomp/comp/local(仅当对应字段为 0xFFFFFFFF 时出现)。
        var extraCursor = cursor + 46 + nameLength;
        var extraEnd = extraCursor + extraLength;
        while (extraCursor + 4 <= extraEnd)
        {
            var headerId = BinaryPrimitives.ReadUInt16LittleEndian(central.AsSpan(extraCursor));
            var dataSize = BinaryPrimitives.ReadUInt16LittleEndian(central.AsSpan(extraCursor + 2));
            var dataStart = extraCursor + 4;
            if (dataStart + dataSize > extraEnd)
            {
                break;
            }

            if (headerId == 0x0001)
            {
                var data = dataStart;
                if (uncompressedSize == 0xFFFF_FFFF && data + 8 <= extraEnd)
                {
                    uncompressedSize = unchecked((long)BinaryPrimitives.ReadUInt64LittleEndian(central.AsSpan(data)));
                    data += 8;
                }

                if (compressedSize == 0xFFFF_FFFF && data + 8 <= extraEnd)
                {
                    compressedSize = unchecked((long)BinaryPrimitives.ReadUInt64LittleEndian(central.AsSpan(data)));
                    data += 8;
                }

                if (localOffset == 0xFFFF_FFFF && data + 8 <= extraEnd)
                {
                    localOffset = unchecked((long)BinaryPrimitives.ReadUInt64LittleEndian(central.AsSpan(data)));
                }
            }

            extraCursor = dataStart + dataSize;
        }

        return (new CentralEntry(fullName, uncompressedSize, compressedSize, crc32, method, localOffset),
            46 + nameLength + extraLength + commentLength);
    }

    private static int FindLastSignature(byte[] data, uint signature)
    {
        var signatureBytes = new byte[4];
        BinaryPrimitives.WriteUInt32LittleEndian(signatureBytes, signature);
        for (var index = data.Length - 4; index >= 0; index--)
        {
            if (data.AsSpan(index, 4).SequenceEqual(signatureBytes))
            {
                return index;
            }
        }

        return -1;
    }

    /// <summary>成员完整路径 → 分区名:取文件名再去扩展名(对齐 Rust strip_extension)。</summary>
    private static string PartitionNameOf(string fullName)
    {
        var fileName = Path.GetFileName(fullName);
        if (string.IsNullOrEmpty(fileName))
        {
            return string.Empty;
        }

        var dot = fileName.LastIndexOf('.');
        return dot > 0 ? fileName[..dot] : fileName;
    }

    private static void TryDelete(string path)
    {
        try
        {
            if (File.Exists(path))
            {
                File.Delete(path);
            }
        }
        catch
        {
            // 清理失败无害:.partial 不会被当成成品使用。
        }
    }

    /// <summary>ZIP CRC-32(ISO-HDLC,多项式 0xEDB88320 反射)。手写表驱动,避免额外依赖。</summary>
    private sealed class Crc32
    {
        private static readonly uint[] Table = BuildTable();
        private uint hash = 0xFFFF_FFFF;

        public static Crc32 Create() => new();

        public void Append(ReadOnlySpan<byte> data)
        {
            foreach (var byteValue in data)
            {
                hash = (hash >> 8) ^ Table[(hash ^ byteValue) & 0xFF];
            }
        }

        public uint GetCurrentHashAsUInt32() => ~hash;

        private static uint[] BuildTable()
        {
            var table = new uint[256];
            for (var index = 0; index < 256; index++)
            {
                var value = (uint)index;
                for (var bit = 0; bit < 8; bit++)
                {
                    value = (value & 1) is not 0 ? 0xEDB8_8320 ^ (value >> 1) : value >> 1;
                }

                table[index] = value;
            }

            return table;
        }
    }

    /// <summary>
    /// 远程压缩数据的顺序读流(每个成员独享,不 Seek):按块发 Range 请求,
    /// 逐块回调进度。deflate/store 解压都是顺序读,无需随机访问。
    /// </summary>
    private sealed class SubRangeStream(
        HttpClient http, string url, long start, long length,
        CancellationToken cancellationToken, Action<string, long>? onChunk, string partitionName) : Stream
    {
        private const int ChunkSize = 512 * 1024;
        private long position;
        private byte[]? buffer;
        private long bufferStart;
        private int bufferLength;

        public override long Length => length;

        public override long Position { get => position; set => position = value; }

        public override bool CanRead => true;

        public override bool CanSeek => true;

        public override bool CanWrite => false;

        public override int Read(byte[] destination, int offset, int count)
        {
            cancellationToken.ThrowIfCancellationRequested();
            if (position >= length)
            {
                return 0;
            }

            var remaining = length - position;
            var requestStart = start + position;
            // 块缓存:同一块内的读取不发新请求。
            if (buffer is null || position < bufferStart || position >= bufferStart + bufferLength)
            {
                var fetchLength = (int)Math.Min(ChunkSize, remaining);
                buffer = FetchAsync(http, url, requestStart, requestStart + fetchLength - 1, cancellationToken).GetAwaiter().GetResult();
                // bufferStart 记录流内相对位置(Read 的 bufferOffset 是相对差值),requestStart 才是绝对偏移。
                bufferStart = position;
                bufferLength = buffer.Length;
                onChunk?.Invoke(partitionName, Math.Min(position + bufferLength, length));
            }

            var bufferOffset = (int)(position - bufferStart);
            var available = bufferLength - bufferOffset;
            var copied = (int)Math.Min(available, Math.Min(count, remaining));
            Array.Copy(buffer!, bufferOffset, destination, offset, copied);
            position += copied;
            return copied;
        }

        public override long Seek(long offset, SeekOrigin origin) => Position = origin switch
        {
            SeekOrigin.Begin => offset,
            SeekOrigin.Current => position + offset,
            SeekOrigin.End => length + offset,
            _ => throw new ArgumentOutOfRangeException(nameof(origin)),
        };

        public override void Flush() { }

        public override void SetLength(long value) => throw new NotSupportedException();

        public override void Write(byte[] buffer, int offset, int count) => throw new NotSupportedException();
    }
}

/// <summary>远程固件格式(对齐 Rust <c>RemoteFirmwareKind</c>)。</summary>
public enum RemoteFirmwareKind
{
    /// <summary>payload.bin 原始镜像(A/B 更新包的 CrAU 容器)。</summary>
    PayloadRaw,

    /// <summary>ZIP 包内含 payload.bin(A/B OTA 包,需 payload_dumper)。</summary>
    PayloadZip,

    /// <summary>ZIP 包直接含分区镜像(线刷/卡刷包)。</summary>
    DirectImageZip,

    /// <summary>无法识别的格式。</summary>
    Unsupported,
}
