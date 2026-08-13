using VivoKsu.App.Models;

namespace VivoKsu.App.Services;

public interface IPartitionTransport
{
    PartitionTransportKind Kind { get; }

    Task<PartitionSnapshot> DiscoverAsync(string serial, CancellationToken cancellationToken);

    Task BackupAsync(
        string serial,
        PartitionTask task,
        IProgress<PartitionTransferProgress>? progress,
        CancellationToken cancellationToken);

    Task WriteAsync(
        string serial,
        PartitionTask task,
        IProgress<PartitionTransferProgress>? progress,
        CancellationToken cancellationToken);

    Task EraseAsync(
        string serial,
        PartitionTask task,
        IProgress<PartitionTransferProgress>? progress,
        CancellationToken cancellationToken);
}
