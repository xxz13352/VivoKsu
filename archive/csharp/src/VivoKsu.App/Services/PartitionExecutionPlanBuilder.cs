using System.IO;
using VivoKsu.App.Models;

namespace VivoKsu.App.Services;

public sealed class PartitionExecutionPlanBuilder
{
    public PartitionExecutionPlan BuildWrite(
        string serial,
        PartitionTransportKind transport,
        IEnumerable<DevicePartition> selectedPartitions,
        IReadOnlyDictionary<string, string> imagePaths)
    {
        var tasks = selectedPartitions
            .Where(partition => imagePaths.TryGetValue(partition.Name, out var imagePath) && !string.IsNullOrWhiteSpace(imagePath))
            .Select(partition => new PartitionTask(
                partition.Name,
                partition.DevicePath,
                imagePaths[partition.Name],
                null,
                partition.SizeBytes))
            .ToArray();

        if (transport == PartitionTransportKind.AdbRoot)
        {
            ValidateAdbRootWriteTasks(tasks);
        }

        return CreatePlan(serial, transport, PartitionOperationKind.Write, tasks);
    }

    public PartitionExecutionPlan BuildBackup(
        string serial,
        PartitionTransportKind transport,
        IEnumerable<DevicePartition> selectedPartitions,
        string outputDirectory)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(outputDirectory);
        var tasks = selectedPartitions
            .Select(partition => new PartitionTask(
                partition.Name,
                partition.DevicePath,
                null,
                Path.Combine(outputDirectory, $"{partition.Name}.img"),
                partition.SizeBytes))
            .ToArray();

        return CreatePlan(serial, transport, PartitionOperationKind.Backup, tasks);
    }

    public PartitionExecutionPlan BuildErase(
        string serial,
        PartitionTransportKind transport,
        IEnumerable<DevicePartition> selectedPartitions)
    {
        var tasks = selectedPartitions
            .Select(partition => new PartitionTask(
                partition.Name,
                partition.DevicePath,
                null,
                null,
                partition.SizeBytes))
            .ToArray();

        return CreatePlan(serial, transport, PartitionOperationKind.Erase, tasks);
    }

    private static PartitionExecutionPlan CreatePlan(
        string serial,
        PartitionTransportKind transport,
        PartitionOperationKind operation,
        IReadOnlyList<PartitionTask> tasks)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(serial);
        if (transport == PartitionTransportKind.Automatic)
        {
            throw new ArgumentException("执行计划必须使用已解析的设备通道。", nameof(transport));
        }

        if (tasks.Count == 0)
        {
            throw new InvalidOperationException("请至少选择一个可执行分区任务。");
        }

        return new PartitionExecutionPlan(serial, transport, operation, tasks);
    }

    private static void ValidateAdbRootWriteTasks(IEnumerable<PartitionTask> tasks)
    {
        Span<byte> header = stackalloc byte[4];
        foreach (var task in tasks)
        {
            var imagePath = task.ImagePath!;
            if (!File.Exists(imagePath))
            {
                throw new InvalidOperationException($"分区 {task.PartitionName} 的镜像文件不存在：{imagePath}");
            }

            var imageLength = new FileInfo(imagePath).Length;
            if (imageLength == 0)
            {
                throw new InvalidOperationException($"分区 {task.PartitionName} 的镜像文件为空：{imagePath}");
            }

            if (task.SizeBytes > 0 && imageLength > task.SizeBytes)
            {
                throw new InvalidOperationException($"分区 {task.PartitionName} 的镜像文件大于分区容量。");
            }

            if (imageLength >= 4)
            {
                using var stream = File.OpenRead(imagePath);
                stream.ReadExactly(header);
                if (header[0] == 0x3A && header[1] == 0xFF && header[2] == 0x26 && header[3] == 0xED)
                {
                    throw new InvalidOperationException($"分区 {task.PartitionName} 的镜像是 Android sparse 镜像，不能通过 ADB Root 直接写入。");
                }
            }
        }
    }
}
