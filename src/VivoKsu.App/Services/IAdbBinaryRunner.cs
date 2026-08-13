using VivoKsu.App.Models;

namespace VivoKsu.App.Services;

public interface IAdbBinaryRunner
{
    Task<string> RunTextAsync(string executable, IReadOnlyList<string> arguments, CancellationToken cancellationToken);

    Task CopyFromDeviceAsync(
        string executable,
        IReadOnlyList<string> arguments,
        string localPath,
        IProgress<PartitionTransferProgress>? progress,
        CancellationToken cancellationToken);

    Task CopyToDeviceAsync(
        string executable,
        IReadOnlyList<string> arguments,
        string localPath,
        IProgress<PartitionTransferProgress>? progress,
        CancellationToken cancellationToken);
}
