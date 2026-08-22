using VivoKsu.App.Models;

namespace VivoKsu.App.Services;

public interface IAdbRootTransferRunner
{
    Task<string> RunRootAsync(string serial, string command, CancellationToken cancellationToken);

    Task CopyFromDeviceAsync(
        string serial,
        string devicePath,
        string localPath,
        IProgress<PartitionTransferProgress>? progress,
        CancellationToken cancellationToken);

    Task CopyToDeviceAsync(
        string serial,
        string localImagePath,
        string devicePath,
        IProgress<PartitionTransferProgress>? progress,
        CancellationToken cancellationToken);

    Task EraseAsync(
        string serial,
        string devicePath,
        IProgress<PartitionTransferProgress>? progress,
        CancellationToken cancellationToken);
}
