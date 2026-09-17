namespace VivoKsu.App.Models;

public enum PartitionTransportKind
{
    Automatic,
    AdbRoot,
    Fastboot
}

public enum PartitionOperationKind
{
    Backup,
    Write,
    Erase
}

public enum PartitionTaskState
{
    Waiting,
    Running,
    Succeeded,
    Failed,
    Canceled
}

public sealed record DevicePartition(
    string Name,
    string DevicePath,
    long? SizeBytes,
    string Slot,
    bool IsMounted,
    bool IsHighRisk,
    bool CanBackup);

public sealed record PartitionSnapshot(
    string Serial,
    PartitionTransportKind Transport,
    string ActiveSlot,
    IReadOnlyList<DevicePartition> Partitions);

public sealed record PartitionTask(
    string PartitionName,
    string DevicePath,
    string? ImagePath,
    string? OutputPath,
    long? SizeBytes);

public sealed record PartitionExecutionPlan(
    string Serial,
    PartitionTransportKind Transport,
    PartitionOperationKind Operation,
    IReadOnlyList<PartitionTask> Tasks);

public sealed record PartitionTransferProgress(
    string PartitionName,
    long TransferredBytes,
    long? TotalBytes,
    double BytesPerSecond);

public sealed class PartitionOperationException : Exception
{
    public PartitionOperationException(
        PartitionTransportKind transport,
        string partitionName,
        string stage,
        Exception innerException)
        : base($"{transport} {partitionName} 在{stage}阶段失败：{innerException.Message}", innerException)
    {
        Transport = transport;
        PartitionName = partitionName;
        Stage = stage;
    }

    public PartitionTransportKind Transport { get; }

    public string PartitionName { get; }

    public string Stage { get; }
}
