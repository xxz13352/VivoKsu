using System.Net;
using System.Security.Cryptography;
using VivoKsu.App.Services;

namespace VivoKsu.App.Tests;

public class RemoteAssetDownloaderTests
{
    private const string GitHubUrl = "https://github.com/xxz13352/NWFlash/releases/download/v1.0.0/KSU.APK";

    [Fact]
    public async Task Downloads_from_direct_url_when_it_succeeds()
    {
        var bytes = new byte[] { 1, 2, 3 };
        using var handler = new TestRoutingHandler().Route("github.com", HttpStatusCode.OK, bytes);
        using var client = new HttpClient(handler);
        var downloader = new RemoteAssetDownloader(client, mirrorList: []);
        var destination = TempFile();

        try
        {
            await downloader.DownloadAsync(new RemoteAssetSpec("测试资产", GitHubUrl), destination, null, CancellationToken.None);
            Assert.Equal(bytes, await File.ReadAllBytesAsync(destination));
            Assert.Single(handler.Requests); // 只尝试直连
        }
        finally
        {
            TryDelete(destination);
        }
    }

    [Fact]
    public async Task Fails_over_to_next_mirror_until_one_succeeds()
    {
        var goodBytes = new byte[] { 9, 9, 9 };
        using var handler = new TestRoutingHandler()
            .Route("github.com", HttpStatusCode.InternalServerError, [])
            .Route("mirror-a.example", HttpStatusCode.NotFound, [])
            .Route("mirror-b.example", HttpStatusCode.OK, goodBytes);
        using var client = new HttpClient(handler);
        var downloader = new RemoteAssetDownloader(client, mirrorList: ["https://mirror-a.example/", "https://mirror-b.example/"]);
        var destination = TempFile();

        try
        {
            await downloader.DownloadAsync(new RemoteAssetSpec("测试资产", GitHubUrl), destination, null, CancellationToken.None);
            Assert.Equal(goodBytes, await File.ReadAllBytesAsync(destination));
            Assert.Equal(
                ["github.com", "mirror-a.example", "mirror-b.example"],
                handler.Requests.Select(request => request.Host).ToArray());
        }
        finally
        {
            TryDelete(destination);
        }
    }

    [Fact]
    public async Task Rejects_content_that_fails_sha256_and_tries_next_source()
    {
        var expectedHash = HashOf([7, 7, 7]);
        var spec = new RemoteAssetSpec("测试资产", GitHubUrl, ExpectedSha256: expectedHash);
        using var handler = new TestRoutingHandler()
            .Route("github.com", HttpStatusCode.OK, [1, 2, 3])
            .Route("mirror.example", HttpStatusCode.OK, [7, 7, 7]);
        using var client = new HttpClient(handler);
        var downloader = new RemoteAssetDownloader(client, mirrorList: ["https://mirror.example/"]);
        var destination = TempFile();

        try
        {
            await downloader.DownloadAsync(spec, destination, null, CancellationToken.None);
            Assert.Equal(new byte[] { 7, 7, 7 }, await File.ReadAllBytesAsync(destination));
            Assert.Equal(2, handler.Requests.Count); // 直连哈希不符 → 换镜像
        }
        finally
        {
            TryDelete(destination);
        }
    }

    [Fact]
    public async Task Rejects_content_with_wrong_length_and_tries_next_source()
    {
        var spec = new RemoteAssetSpec("测试资产", GitHubUrl, ExpectedLength: 3);
        using var handler = new TestRoutingHandler()
            .Route("github.com", HttpStatusCode.OK, [1, 2])
            .Route("mirror.example", HttpStatusCode.OK, [1, 2, 3]);
        using var client = new HttpClient(handler);
        var downloader = new RemoteAssetDownloader(client, mirrorList: ["https://mirror.example/"]);
        var destination = TempFile();

        try
        {
            await downloader.DownloadAsync(spec, destination, null, CancellationToken.None);
            Assert.Equal(new byte[] { 1, 2, 3 }, await File.ReadAllBytesAsync(destination));
        }
        finally
        {
            TryDelete(destination);
        }
    }

    [Fact]
    public async Task Throws_with_manual_link_when_all_candidates_fail()
    {
        using var handler = new TestRoutingHandler()
            .Route("github.com", HttpStatusCode.NotFound, [])
            .Route("mirror.example", HttpStatusCode.GatewayTimeout, []);
        using var client = new HttpClient(handler);
        var downloader = new RemoteAssetDownloader(client, mirrorList: ["https://mirror.example/"]);
        var destination = TempFile();

        try
        {
            var exception = await Assert.ThrowsAsync<RemoteAssetDownloadException>(() =>
                downloader.DownloadAsync(new RemoteAssetSpec("测试资产", GitHubUrl), destination, null, CancellationToken.None));
            Assert.Contains(GitHubUrl, exception.Message); // 手动下载链接兜底
        }
        finally
        {
            TryDelete(destination);
        }
    }

    [Fact]
    public async Task Propagates_user_cancellation_without_trying_mirrors()
    {
        using var handler = new TestRoutingHandler();
        using var client = new HttpClient(handler);
        var downloader = new RemoteAssetDownloader(client, mirrorList: ["https://mirror.example/"]);
        using var cts = new CancellationTokenSource();
        cts.Cancel();
        var destination = TempFile();

        try
        {
            await Assert.ThrowsAnyAsync<OperationCanceledException>(() =>
                downloader.DownloadAsync(new RemoteAssetSpec("测试资产", GitHubUrl), destination, null, cts.Token));
            Assert.Empty(handler.Requests); // 用户取消不发起任何网络请求
        }
        finally
        {
            TryDelete(destination);
        }
    }

    [Fact]
    public async Task No_progress_watchdog_aborts_a_stalled_candidate()
    {
        // 镜像「发了头但 body 永不送达」:看门狗(短超时 2s)应在数十秒内放弃,而非无限挂起。
        using var handler = new StalledResponseHandler();
        using var client = new HttpClient(handler);
        var downloader = new RemoteAssetDownloader(client, mirrorList: ["https://mirror.example/"], noProgressTimeout: TimeSpan.FromSeconds(2));
        var destination = TempFile();

        try
        {
            var started = DateTime.UtcNow;
            var exception = await Assert.ThrowsAsync<RemoteAssetDownloadException>(() =>
                downloader.DownloadAsync(new RemoteAssetSpec("卡死源", GitHubUrl), destination, null, CancellationToken.None));
            var elapsed = DateTime.UtcNow - started;

            Assert.NotNull(exception);
            Assert.True(elapsed < TimeSpan.FromSeconds(60), $"看门狗未生效,耗时 {elapsed.TotalSeconds:0.0}s");
        }
        finally
        {
            TryDelete(destination);
        }
    }

    private static string HashOf(byte[] content) =>
        Convert.ToHexString(SHA256.HashData(content)).ToLowerInvariant();

    private static string TempFile() =>
        Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"), "out.bin");

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
            // Best effort.
        }
    }
}

/// <summary>返回响应头 + Content-Length,但 body 永不送达(读阻塞到取消)——模拟镜像「连上但不出数据」。</summary>
public sealed class StalledResponseHandler : HttpMessageHandler
{
    protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken)
    {
        var response = new HttpResponseMessage(HttpStatusCode.OK)
        {
            Content = new StreamContent(new StalledStream())
        };
        response.Content.Headers.ContentLength = 1_000_000;
        return Task.FromResult(response);
    }

    private sealed class StalledStream : Stream
    {
        public override bool CanRead => true;
        public override bool CanSeek => false;
        public override bool CanWrite => false;
        public override long Length => throw new NotSupportedException();
        public override long Position { get => 0; set => throw new NotSupportedException(); }
        public override void Flush() { }
        public override int Read(byte[] buffer, int offset, int count) => throw new NotSupportedException();
        public override long Seek(long offset, SeekOrigin origin) => throw new NotSupportedException();
        public override void SetLength(long value) => throw new NotSupportedException();
        public override void Write(byte[] buffer, int offset, int count) => throw new NotSupportedException();

        public override ValueTask<int> ReadAsync(Memory<byte> buffer, CancellationToken cancellationToken = default)
        {
            // 阻塞到取消(看门狗取消时抛 OCE),模拟服务器发了头后 body 永不送达。
            var tcs = new TaskCompletionSource<int>(TaskCreationOptions.RunContinuationsAsynchronously);
            cancellationToken.Register(() => tcs.SetCanceled(cancellationToken));
            return new ValueTask<int>(tcs.Task);
        }
    }
}
