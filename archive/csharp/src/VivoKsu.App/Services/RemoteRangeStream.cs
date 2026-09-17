using System.IO;
using System.Net.Http;

namespace VivoKsu.App.Services;

/// <summary>
/// 把支持 Range 的 HTTP 远程文件伪装成本地可随机访问的 <see cref="Stream"/>
/// (对齐 Rust <c>RangeHttpReader</c>):ZipArchive 据此可以直接对远程 ZIP 建档,
/// 只拉取中央目录和目标成员的字节,不用下载整包。
/// <para>
/// 读策略:512 KiB 块缓存。ZipArchive 建档时的 EOCD/中央目录小读取被合并;
/// 提取大成员时几乎是顺序读,块越大网络往返越少。
/// </para>
/// </summary>
public sealed class RemoteRangeStream : Stream
{
    /// <summary>单块预取大小:建档阶段的小 Seek 合并进同一块,提取阶段顺序读每块一个请求。</summary>
    private const int DefaultChunkSize = 512 * 1024;

    private readonly HttpClient http;
    private readonly string url;
    private readonly CancellationToken cancellationToken;
    private readonly Action<long>? onBytesDownloaded;
    private readonly byte[] buffer;
    private long bufferStart;
    private int bufferLength;
    private long position;

    /// <param name="onBytesDownloaded">每次从网络拉到新字节时回调(累计字节数;进度展示用)。</param>
    public RemoteRangeStream(HttpClient http, string url, CancellationToken cancellationToken = default, int chunkSize = DefaultChunkSize, Action<long>? onBytesDownloaded = null)
    {
        this.http = http ?? throw new ArgumentNullException(nameof(http));
        this.url = ValidateUrl(url);
        this.cancellationToken = cancellationToken;
        this.onBytesDownloaded = onBytesDownloaded;
        buffer = new byte[chunkSize];
        Length = ProbeLength();
    }

    public override long Length { get; }

    public override long Position
    {
        get => position;
        set
        {
            if (value is < 0 || value > Length)
            {
                throw new ArgumentOutOfRangeException(nameof(value), "读取位置超出远程文件范围。");
            }

            position = value;
        }
    }

    public override bool CanRead => true;

    public override bool CanSeek => true;

    public override bool CanWrite => false;

    public override int Read(byte[] destination, int offset, int count)
    {
        ArgumentNullException.ThrowIfNull(destination);
        if ((uint)offset > destination.Length || count < 0 || offset + count > destination.Length)
        {
            throw new ArgumentOutOfRangeException(nameof(offset), "读取缓冲区越界。");
        }

        if (count == 0 || position >= Length)
        {
            return 0;
        }

        EnsureBuffered(position);
        var bufferOffset = (int)(position - bufferStart);
        var available = bufferLength - bufferOffset;
        var copied = Math.Min(available, count);
        Array.Copy(buffer, bufferOffset, destination, offset, copied);
        position += copied;
        return copied;
    }

    public override long Seek(long offset, SeekOrigin origin) => Position = origin switch
    {
        SeekOrigin.Begin => offset,
        SeekOrigin.Current => position + offset,
        SeekOrigin.End => Length + offset,
        _ => throw new ArgumentOutOfRangeException(nameof(origin)),
    };

    public override void Flush() { }

    public override void SetLength(long value) => throw new NotSupportedException();

    public override void Write(byte[] buffer, int offset, int count) => throw new NotSupportedException();

    /// <summary>确保当前读取位置落在缓冲块内;不在则从网络按块拉取。</summary>
    private void EnsureBuffered(long target)
    {
        if (bufferLength > 0 && target >= bufferStart && target < bufferStart + bufferLength)
        {
            return;
        }

        var start = target;
        var end = Math.Min(start + buffer.Length, Length) - 1;
        var fetched = FetchRange(start, end);
        if (fetched.Length == 0)
        {
            throw new IOException("远程文件读取返回空响应。");
        }

        Array.Copy(fetched, buffer, fetched.Length);
        bufferStart = start;
        bufferLength = fetched.Length;
    }

    private byte[] FetchRange(long start, long endInclusive)
    {
        using var request = new HttpRequestMessage(HttpMethod.Get, url);
        request.Headers.Range = new System.Net.Http.Headers.RangeHeaderValue(start, endInclusive);
        using var response = http.Send(request, HttpCompletionOption.ResponseHeadersRead, cancellationToken);
        cancellationToken.ThrowIfCancellationRequested();

        // 服务器不支持 Range(返回 200 全量)时直接失败:整包下载走 OtaDownloadService,不在这里悄悄退化。
        if (response.StatusCode != System.Net.HttpStatusCode.PartialContent)
        {
            throw new IOException("固件服务器不支持分段下载(未返回 206),请改用手动选择镜像或整包下载。");
        }

        using var stream = response.Content.ReadAsStream(cancellationToken);
        using var output = new MemoryStream();
        stream.CopyTo(output, 64 * 1024);
        cancellationToken.ThrowIfCancellationRequested();

        var bytes = output.ToArray();
        if (bytes.Length != (int)(endInclusive - start + 1))
        {
            throw new IOException($"远程读取返回长度不符:期望 {endInclusive - start + 1} 字节,实际 {bytes.Length} 字节。");
        }

        onBytesDownloaded?.Invoke(bytes.Length);
        return bytes;
    }

    /// <summary>用 Range(0,0) 探测总长度(206 响应的 Content-Range 尾段),顺带验证服务器支持分段。</summary>
    private long ProbeLength()
    {
        using var request = new HttpRequestMessage(HttpMethod.Get, url);
        request.Headers.Range = new System.Net.Http.Headers.RangeHeaderValue(0, 0);
        using var response = http.Send(request, HttpCompletionOption.ResponseHeadersRead, cancellationToken);
        cancellationToken.ThrowIfCancellationRequested();

        if (response.StatusCode != System.Net.HttpStatusCode.PartialContent)
        {
            throw new IOException("固件服务器不支持分段下载(未返回 206),请改用手动选择镜像或整包下载。");
        }

        var contentRange = response.Content.Headers.ContentRange?.Length;
        if (contentRange is not > 0)
        {
            throw new IOException("固件服务器未返回文件大小(Content-Range 缺失)。");
        }

        return contentRange.Value;
    }

    /// <summary>URL 校验(对齐 Rust validate_http_url):仅允许 http/https。</summary>
    internal static string ValidateUrl(string url)
    {
        if (string.IsNullOrWhiteSpace(url)
            || !Uri.TryCreate(url.Trim(), UriKind.Absolute, out var parsed)
            || parsed.Scheme is not ("http" or "https")
            || string.IsNullOrEmpty(parsed.Host))
        {
            throw new ArgumentException("固件地址必须是有效的 HTTP 或 HTTPS URL。", nameof(url));
        }

        return url.Trim();
    }
}
