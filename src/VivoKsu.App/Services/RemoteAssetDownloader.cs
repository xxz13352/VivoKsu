using System.IO;
using System.Net.Http;
using System.Security.Cryptography;
using VivoKsu.App.Models;

namespace VivoKsu.App.Services;

/// <summary>一个可下载的远程资产:名称、GitHub Release 直链、可选的 SHA-256 / 长度校验。</summary>
public sealed record RemoteAssetSpec(
    string DisplayName,
    string GitHubUrl,
    string? ExpectedSha256 = null,
    long? ExpectedLength = null);

/// <summary>所有候选源(直连 + 各镜像)均失败时抛出,消息含手动下载链接。</summary>
public sealed class RemoteAssetDownloadException : Exception
{
    public RemoteAssetDownloadException(RemoteAssetSpec spec, string manualUrl, Exception? inner)
        : base($"下载 {spec.DisplayName} 失败。请手动下载后放入提示的位置,或点击以下链接手动获取:\n{manualUrl}", inner)
    {
        Spec = spec;
    }

    public RemoteAssetSpec Spec { get; }
}

public interface IRemoteAssetDownloader
{
    /// <summary>
    /// 下载 <paramref name="spec"/> 到 <paramref name="destinationPath"/>。候选源顺序:
    /// 直连 → <see cref="RemoteAssetCatalog.Mirrors"/> 各镜像,成功即返回;全部失败抛
    /// <see cref="RemoteAssetDownloadException"/>。下载到 staging 后做长度/SHA-256 校验,
    /// 通过才原子移动到目标路径。进度按 <see cref="DownloadProgress"/> 上报(字节 + 总字节)。
    /// </summary>
    Task DownloadAsync(
        RemoteAssetSpec spec,
        string destinationPath,
        IProgress<DownloadProgress>? progress,
        CancellationToken cancellationToken);
}

/// <summary>
/// GitHub Release 资产的按需下载器,带多镜像 failover。国内直连 github 不稳(2026-08-15
/// 实测直接超时),镜像逐个兜底,全部失败给出手动下载链接而非静默吞错。
/// </summary>
public sealed class RemoteAssetDownloader : IRemoteAssetDownloader
{
    /// <summary>单候选源超时(含镜像);用户取消不受此限。</summary>
    private static readonly TimeSpan PerCandidateTimeout = TimeSpan.FromMinutes(3);

    private readonly HttpClient httpClient;
    private readonly IReadOnlyList<string> mirrorList;

    public RemoteAssetDownloader(HttpClient? httpClient = null, IEnumerable<string>? mirrorList = null)
    {
        this.httpClient = httpClient ?? new HttpClient { Timeout = Timeout.InfiniteTimeSpan };
        this.mirrorList = mirrorList?.ToArray() ?? RemoteAssetCatalog.Mirrors;
        if (!this.httpClient.DefaultRequestHeaders.UserAgent.Any())
        {
            this.httpClient.DefaultRequestHeaders.UserAgent.ParseAdd("VivoKsu-App/1.0");
        }
    }

    public async Task DownloadAsync(
        RemoteAssetSpec spec,
        string destinationPath,
        IProgress<DownloadProgress>? progress,
        CancellationToken cancellationToken)
    {
        Directory.CreateDirectory(Path.GetDirectoryName(Path.GetFullPath(destinationPath))!);
        Exception? lastError = null;
        foreach (var candidate in BuildCandidates(spec.GitHubUrl))
        {
            // HttpClient 对「调用前已取消」的 token 不保证立刻抛 OCE(实测会照常发请求),
            // 显式检查保证用户取消确定性传播,不做无谓的镜像重试。
            cancellationToken.ThrowIfCancellationRequested();

            var stagingPath = destinationPath + ".staging";
            try
            {
                using var linked = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
                linked.CancelAfter(PerCandidateTimeout);
                await DownloadFromAsync(candidate, stagingPath, progress, linked.Token);
                Verify(spec, stagingPath);
                File.Move(stagingPath, destinationPath, overwrite: true);
                return;
            }
            catch (OperationCanceledException) when (cancellationToken.IsCancellationRequested)
            {
                // 用户主动取消:直接传播,不再尝试后续镜像。
                throw;
            }
            catch (Exception exception)
            {
                lastError = exception;
                TryDelete(stagingPath);
            }
        }

        throw new RemoteAssetDownloadException(spec, spec.GitHubUrl, lastError);
    }

    private IEnumerable<string> BuildCandidates(string githubUrl)
    {
        yield return githubUrl;
        foreach (var mirror in mirrorList)
        {
            yield return mirror.TrimEnd('/') + "/" + githubUrl;
        }
    }

    private async Task DownloadFromAsync(
        string url,
        string stagingPath,
        IProgress<DownloadProgress>? progress,
        CancellationToken cancellationToken)
    {
        using var response = await httpClient.GetAsync(url, HttpCompletionOption.ResponseHeadersRead, cancellationToken);
        response.EnsureSuccessStatusCode();
        var totalBytes = response.Content.Headers.ContentLength;
        await using var input = await response.Content.ReadAsStreamAsync(cancellationToken);
        await using var output = File.Create(stagingPath);
        var buffer = new byte[81920];
        long read = 0L;
        long lastBytes = 0L;
        long lastTick = Environment.TickCount64;
        double bytesPerSecond = 0;
        int count;
        while ((count = await input.ReadAsync(buffer, cancellationToken)) > 0)
        {
            await output.WriteAsync(buffer.AsMemory(0, count), cancellationToken);
            read += count;
            // 速度按 ~250ms 采样一次,避免每次 ReadAsync 都算(高频无意义)。
            var now = Environment.TickCount64;
            var elapsed = now - lastTick;
            if (elapsed >= 250)
            {
                bytesPerSecond = (read - lastBytes) * 1000.0 / elapsed;
                lastBytes = read;
                lastTick = now;
            }

            progress?.Report(new DownloadProgress(read, totalBytes, bytesPerSecond));
        }
    }

    /// <summary>下载完成后校验:长度与 SHA-256(可选设置)。任一不符视为该源内容坏,抛错换下一源。</summary>
    private static void Verify(RemoteAssetSpec spec, string path)
    {
        var info = new FileInfo(path);
        if (spec.ExpectedLength is { } expectedLength && info.Length != expectedLength)
        {
            throw new InvalidDataException(
                $"{spec.DisplayName} 长度不符(期望 {expectedLength} 字节,实际 {info.Length})。");
        }

        if (spec.ExpectedSha256 is { } expectedHash)
        {
            using var stream = File.OpenRead(path);
            var actualHash = Convert.ToHexString(SHA256.HashData(stream)).ToLowerInvariant();
            if (!string.Equals(actualHash, expectedHash, StringComparison.OrdinalIgnoreCase))
            {
                throw new InvalidDataException($"{spec.DisplayName} 完整性校验失败(SHA-256 不匹配)。");
            }
        }
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
            // Best effort; 下次下载的 staging 覆盖。
        }
    }
}
