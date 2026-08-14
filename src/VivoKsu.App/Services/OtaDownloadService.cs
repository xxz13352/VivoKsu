using System.ComponentModel;
using System.IO;
using Downloader;
using VivoKsu.App.Models;

namespace VivoKsu.App.Services;

/// <summary>
/// OTA 包多分片下载。底层使用 GitHub 上的 <c>bezzad/Downloader</c> 库
/// (NuGet: Downloader, v5.9.5) —— 成熟的多连接 Range 下载实现,本类只做薄封装:
/// 解析总大小、校验磁盘空间、配置分片范围、把进度事件转成 <see cref="DownloadProgress"/>、转发取消。
/// </summary>
public sealed class OtaDownloadService : IDisposable
{
    /// <summary>bezzad 分片下载的内存缓冲上限(字节)。必须 >0,否则无界堆积导致 OOM,见 BuildConfiguration 注释。</summary>
    internal const long MaxMemoryBufferBytes = 256L * 1024 * 1024;

    /// <summary>进度上报最小间隔:高速下载时事件可达每秒上千次,节流后界面按 ~10Hz 刷新。</summary>
    internal const long ProgressReportIntervalMs = 100;

    private readonly int chunkCount;

    public OtaDownloadService(int chunkCount = 8)
    {
        this.chunkCount = chunkCount;
    }

    public Task DownloadAsync(
        Uri url,
        string destinationPath,
        IProgress<DownloadProgress>? progress,
        CancellationToken cancellationToken)
    {
        ArgumentNullException.ThrowIfNull(url);
        ArgumentException.ThrowIfNullOrWhiteSpace(destinationPath);

        var directory = Path.GetDirectoryName(destinationPath);
        if (!string.IsNullOrWhiteSpace(directory))
        {
            Directory.CreateDirectory(directory);
        }

        return DownloadCoreAsync(url, destinationPath, progress, cancellationToken);
    }

    /// <summary>把库的进度事件参数映射为应用内的 <see cref="DownloadProgress"/>。</summary>
    public static DownloadProgress MapProgress(DownloadProgressChangedEventArgs e) =>
        new(
            e.ReceivedBytesSize,
            e.TotalBytesToReceive > 0 ? e.TotalBytesToReceive : null,
            e.BytesPerSecondSpeed);

    /// <summary>
    /// 进度节流:下载完成(已收字节 ≥ 总量)必须上报;否则只有距上次上报超过
    /// <paramref name="minIntervalMs"/> 时才上报并刷新计时,避免高速下载时把
    /// 数以千计的进度事件灌进 UI 线程队列造成卡死。
    /// </summary>
    internal static bool ShouldReportProgress(ref long lastReportTicks, DownloadProgressChangedEventArgs e, long minIntervalMs)
    {
        var now = Environment.TickCount64;
        var isComplete = e.TotalBytesToReceive > 0 && e.ReceivedBytesSize >= e.TotalBytesToReceive;
        if (!isComplete && now - lastReportTicks < minIntervalMs)
        {
            return false;
        }

        lastReportTicks = now;
        return true;
    }

    /// <summary>
    /// 根据服务端信息构建下载配置。
    /// 注意 bezzad 5.9.5 的坑:开 <c>RangeDownload=true</c> 时若不显式设置
    /// <c>RangeHigh</c>,<c>SetRangedSizes</c> 会把 TotalFileSize 算成
    /// <c>RangeHigh - RangeLow + 1 = 1</c>,导致只下载 1 字节就"完成"
    /// (Vivo CDN 的 OTA 链接实测必现)。因此必须先用 HEAD/探测拿到总大小,
    /// 再设 <c>RangeLow=0、RangeHigh=总大小-1</c>。
    /// </summary>
    public static DownloadConfiguration BuildConfiguration(RemoteFileInfo remote, int chunkCount = 8)
    {
        var config = new DownloadConfiguration
        {
            ChunkCount = chunkCount,
            ParallelDownload = true,
            ParallelCount = chunkCount,
            RangeDownload = remote.SupportsRange,
            MinimumSizeOfChunking = 1 << 20, // 小于 1MB 不分片,直接单流
            BufferBlockSize = 1 << 20,
            // 内存缓冲必须有上限:bezzad 的 ConcurrentPacketBuffer 在参数 <=0 时视为
            // 无上限(BufferSize=long.MaxValue),8 分片在网速快于磁盘写入时会无界堆积
            // 1MB 的 Packet——实测下载 1GB 包峰值内存冲到 6.6GB,真实 3-6GB OTA 必然
            // OOM 闪退 + GC 停顿卡死。设 256MB 上限后,队列被背压钳在几百 MB 量级
            // (与包大小无关),健康磁盘上 watcher 持续落盘,不触发下载停滞。
            MaximumMemoryBufferBytes = MaxMemoryBufferBytes,
            MaxTryAgainOnFailure = 3,
            FileExistPolicy = FileExistPolicy.Delete,
            ClearPackageOnCompletionWithFailure = true,
            EnableAutoResumeDownload = true,
            // 磁盘空间由 DownloadAsync 自行预检(带中文提示),这里关闭库的英文检查。
            CheckDiskSizeBeforeDownload = false,
        };

        if (remote.SupportsRange)
        {
            config.RangeLow = 0;
            config.RangeHigh = remote.FileSize - 1;
        }
        else
        {
            // 服务器不支持 Range:退化为单连接整包下载。
            config.ChunkCount = 1;
        }

        return config;
    }

    private async Task DownloadCoreAsync(
        Uri url,
        string destinationPath,
        IProgress<DownloadProgress>? progress,
        CancellationToken cancellationToken)
    {
        // 先解析远端总大小与 Range 支持(bezzad 自己的探测,含重定向处理)。
        var remote = await RemoteFileResolver.GetFileInfoAsync(url.ToString(), null, cancellationToken);
        if (remote.FileSize <= 0)
        {
            throw new IOException($"无法确定 OTA 包大小(服务器未返回 Content-Length): {url}");
        }

        // OTA 包动辄数 GB,下载前先确认磁盘放得下。
        EnsureDiskSpace(destinationPath, remote.FileSize);

        // 每次下载新建 DownloadService,避免复用一个实例残留上次的包状态。
        using var service = new DownloadService(BuildConfiguration(remote, chunkCount));

        // 高速下载时 bezzad 每个 1MB 分片读取都触发一次进度事件(8 分片可达每秒数千次),
        // 直接经 Progress<T> 转发会把 UI 线程的 Dispatcher 消息队列冲爆,界面卡死。
        // 按 ~100ms 节流,只在间隔足够或事件已到达总量(下载完成)时上报。
        long lastProgressReportTicks = 0;
        var handler = new EventHandler<DownloadProgressChangedEventArgs>((_, e) =>
        {
            if (progress is null)
            {
                return;
            }

            if (ShouldReportProgress(ref lastProgressReportTicks, e, ProgressReportIntervalMs))
            {
                progress.Report(MapProgress(e));
            }
        });
        AsyncCompletedEventArgs? completed = null;
        var completionHandler = new EventHandler<AsyncCompletedEventArgs>((_, e) => completed = e);
        service.DownloadProgressChanged += handler;
        service.DownloadFileCompleted += completionHandler;
        try
        {
            await service.DownloadFileTaskAsync(url.ToString(), destinationPath, cancellationToken);
        }
        catch (Exception exception) when (exception is not OperationCanceledException)
        {
            throw new IOException($"下载 OTA 包失败: {exception.Message}", exception);
        }
        finally
        {
            service.DownloadProgressChanged -= handler;
            service.DownloadFileCompleted -= completionHandler;
        }

        // bezzad 的下载失败只投递到 DownloadFileCompleted 事件,不会抛给调用方;
        // 若不检查,失败(如磁盘写满)也会被当成"成功"。
        if (completed is { Error: not null })
        {
            throw new IOException($"下载 OTA 包失败: {completed.Error.Message}", completed.Error);
        }

        if (completed?.Cancelled == true)
        {
            throw new OperationCanceledException("下载 OTA 包已取消");
        }
    }

    /// <summary>校验目标磁盘有足够空间容纳本次下载,不足时抛出带中文说明的异常。</summary>
    public static void EnsureDiskSpace(string destinationPath, long requiredBytes)
    {
        var root = Path.GetPathRoot(Path.GetFullPath(destinationPath));
        if (string.IsNullOrWhiteSpace(root))
        {
            return;
        }

        var drive = new DriveInfo(root);
        if (drive.IsReady && drive.AvailableFreeSpace < requiredBytes)
        {
            throw new IOException(
                $"磁盘空间不足:需要 {FormatBytes(requiredBytes)},驱动器 {drive.Name} 当前可用 {FormatBytes(drive.AvailableFreeSpace)}。请清理磁盘后重试。");
        }
    }

    private static string FormatBytes(long bytes) => bytes switch
    {
        < 1024 => $"{bytes} B",
        < 1024 * 1024 => $"{bytes / 1024d:F1} KB",
        < 1024L * 1024 * 1024 => $"{bytes / 1024d / 1024:F1} MB",
        _ => $"{bytes / 1024d / 1024 / 1024:F2} GB"
    };

    public void Dispose()
    {
    }
}
