using VivoKsu.App.Models;

namespace VivoKsu.App.Services;

public sealed class FastbootPartitionTransport : IPartitionTransport
{
    private readonly FastbootRsBackend backend;

    public FastbootPartitionTransport(FastbootRsBackend backend)
    {
        this.backend = backend;
    }

    public PartitionTransportKind Kind => PartitionTransportKind.Fastboot;

    public async Task<PartitionSnapshot> DiscoverAsync(string serial, CancellationToken cancellationToken)
    {
        try
        {
            var output = await backend.GetVarAsync(serial, "all", cancellationToken);
            return FastbootPartitionTableParser.Parse(serial, output);
        }
        catch (Exception exception) when (exception is not OperationCanceledException)
        {
            throw new PartitionOperationException(Kind, "分区表", "读取", exception);
        }
    }

    public async Task BackupAsync(
        string serial,
        PartitionTask task,
        IProgress<PartitionTransferProgress>? progress,
        CancellationToken cancellationToken)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(task.OutputPath);
        var startedAt = DateTimeOffset.UtcNow;

        try
        {
            var transferred = await backend.FetchAsync(serial, task.PartitionName, task.OutputPath, cancellationToken);
            cancellationToken.ThrowIfCancellationRequested();
            progress?.Report(CreateCompletedProgress(task, transferred, startedAt));
        }
        catch (Exception exception) when (exception is not OperationCanceledException)
        {
            throw new PartitionOperationException(Kind, task.PartitionName, "读取", exception);
        }
    }

    public async Task WriteAsync(
        string serial,
        PartitionTask task,
        IProgress<PartitionTransferProgress>? progress,
        CancellationToken cancellationToken)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(task.ImagePath);
        var startedAt = DateTimeOffset.UtcNow;

        try
        {
            await backend.FlashAsync(serial, task.PartitionName, task.ImagePath, cancellationToken);
            cancellationToken.ThrowIfCancellationRequested();
            progress?.Report(CreateCompletedProgress(task, task.SizeBytes ?? 0, startedAt));
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
            await backend.EraseAsync(serial, task.PartitionName, cancellationToken);
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
