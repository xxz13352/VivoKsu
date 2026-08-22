using System.IO;
using VivoKsu.App.Models;

namespace VivoKsu.App.Services;

public sealed class FastbootPartitionTransport : IPartitionTransport
{
    private readonly IFastbootCliRunner cliRunner;

    public FastbootPartitionTransport(IFastbootCliRunner cliRunner)
    {
        this.cliRunner = cliRunner;
    }

    public PartitionTransportKind Kind => PartitionTransportKind.Fastboot;

    public async Task<PartitionSnapshot> DiscoverAsync(string serial, CancellationToken cancellationToken)
    {
        try
        {
            var output = await cliRunner.GetVarAsync(serial, "all", cancellationToken);
            return FastbootPartitionTableParser.Parse(serial, output);
        }
        catch (Exception exception) when (exception is not OperationCanceledException)
        {
            throw new PartitionOperationException(Kind, "分区表", "读取", exception);
        }
    }

    public Task BackupAsync(
        string serial,
        PartitionTask task,
        IProgress<PartitionTransferProgress>? progress,
        CancellationToken cancellationToken)
    {
        // fastboot 模式本身不支持备份 / 回读分区(fastboot 协议没有读分区命令),
        // 备份应走 ADB Root 通道的 dd。明确报错,而不是让上层误以为成功。
        throw new PartitionOperationException(
            Kind,
            task.PartitionName,
            "读取",
            new NotSupportedException("fastboot 模式不支持备份/回读分区，请切换到 ADB Root 通道。"));
    }

    public async Task WriteAsync(
        string serial,
        PartitionTask task,
        IProgress<PartitionTransferProgress>? progress,
        CancellationToken cancellationToken)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(task.ImagePath);
        var startedAt = DateTimeOffset.UtcNow;
        var totalBytes = task.SizeBytes ?? new FileInfo(task.ImagePath).Length;

        try
        {
            // fastboot flash 带连续传输进度(经 GetProcessIoCounters 采样),适配成
            // 分区传输进度(分区名 + 已传字节 + 速度)。
            long lastBytes = 0;
            var lastReport = startedAt;
            var cliProgress = new Progress<double>(fraction =>
            {
                var transferred = (long)(totalBytes * Math.Clamp(fraction, 0d, 1d));
                var now = DateTimeOffset.UtcNow;
                var seconds = Math.Max((now - lastReport).TotalSeconds, 0.001);
                var speed = (transferred - lastBytes) / seconds;
                lastBytes = transferred;
                lastReport = now;
                progress?.Report(new PartitionTransferProgress(task.PartitionName, transferred, totalBytes, speed));
            });

            await cliRunner.FlashAsync(serial, task.PartitionName, task.ImagePath, cliProgress, cancellationToken);
            cancellationToken.ThrowIfCancellationRequested();
            progress?.Report(CreateCompletedProgress(task, totalBytes, startedAt));
        }
        catch (Exception exception) when (exception is not OperationCanceledException)
        {
            throw new PartitionOperationException(Kind, task.PartitionName, "写入", exception);
        }
    }

    public async Task EraseAsync(
        string serial,
        PartitionTask task,
        IProgress<PartitionTransferProgress>? progress,
        CancellationToken cancellationToken)
    {
        var startedAt = DateTimeOffset.UtcNow;

        try
        {
            await cliRunner.EraseAsync(serial, task.PartitionName, cancellationToken);
            cancellationToken.ThrowIfCancellationRequested();
            progress?.Report(CreateCompletedProgress(task, task.SizeBytes ?? 0, startedAt));
        }
        catch (Exception exception) when (exception is not OperationCanceledException)
        {
            throw new PartitionOperationException(Kind, task.PartitionName, "擦除", exception);
        }
    }

    private static PartitionTransferProgress CreateCompletedProgress(
        PartitionTask task,
        long transferredBytes,
        DateTimeOffset startedAt)
    {
        var elapsedSeconds = Math.Max((DateTimeOffset.UtcNow - startedAt).TotalSeconds, 0.001);
        return new PartitionTransferProgress(
            task.PartitionName,
            transferredBytes,
            task.SizeBytes,
            transferredBytes / elapsedSeconds);
    }
}
