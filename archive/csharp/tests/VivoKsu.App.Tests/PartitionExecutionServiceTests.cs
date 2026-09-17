using FluentAssertions;
using VivoKsu.App.Models;
using VivoKsu.App.Services;
using VivoKsu.App.ViewModels;

namespace VivoKsu.App.Tests;

public class PartitionExecutionServiceTests
{
    [Fact]
    public async Task ExecuteAsync_stops_after_the_first_failed_partition()
    {
        var session = CreateFastbootSession("FAST123");
        var logs = new OperationLogService();
        var transport = new RecordingPartitionTransport(failOn: "init_boot_a");
        var service = new PartitionExecutionService(
            session,
            new OperationCoordinator(session, logs),
            logs,
            [transport]);
        var plan = new PartitionExecutionPlan(
            "FAST123",
            PartitionTransportKind.Fastboot,
            PartitionOperationKind.Write,
            [
                new PartitionTask("boot_a", "boot_a", @"D:\images\boot.img", null, 64),
                new PartitionTask("init_boot_a", "init_boot_a", @"D:\images\init_boot.img", null, 8),
                new PartitionTask("vendor_boot_a", "vendor_boot_a", @"D:\images\vendor_boot.img", null, 96)
            ]);
        var states = new List<(string Name, PartitionTaskState State)>();

        await Assert.ThrowsAsync<PartitionOperationException>(() => service.ExecuteAsync(
            plan,
            (name, state) => states.Add((name, state)),
            _ => { },
            CancellationToken.None));

        transport.Writes.Should().Equal("boot_a", "init_boot_a");
        states.Should().Contain(("boot_a", PartitionTaskState.Succeeded));
        states.Should().Contain(("init_boot_a", PartitionTaskState.Failed));
        states.Should().NotContain(item => item.Name == "vendor_boot_a" && item.State == PartitionTaskState.Running);
    }

    [Fact]
    public async Task ExecuteAsync_renames_a_completed_backup_partial_file()
    {
        var root = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(root);

        try
        {
            var session = CreateFastbootSession("FAST123");
            var logs = new OperationLogService();
            var transport = new RecordingPartitionTransport();
            var service = new PartitionExecutionService(
                session,
                new OperationCoordinator(session, logs),
                logs,
                [transport]);
            var outputPath = Path.Combine(root, "boot_a.img");
            var plan = new PartitionExecutionPlan(
                "FAST123",
                PartitionTransportKind.Fastboot,
                PartitionOperationKind.Backup,
                [new PartitionTask("boot_a", "boot_a", null, outputPath, 64)]);

            await service.ExecuteAsync(plan, (_, _) => { }, _ => { }, CancellationToken.None);

            File.Exists(outputPath).Should().BeTrue();
            File.Exists(outputPath + ".partial").Should().BeFalse();
        }
        finally
        {
            Directory.Delete(root, recursive: true);
        }
    }

    [Fact]
    public async Task ExecuteAsync_rejects_a_truncated_backup_without_overwriting()
    {
        var root = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(root);

        try
        {
            var session = CreateFastbootSession("FAST123");
            var logs = new OperationLogService();
            var transport = new RecordingPartitionTransport { TruncateBackup = true };
            var service = new PartitionExecutionService(
                session,
                new OperationCoordinator(session, logs),
                logs,
                [transport]);
            var outputPath = Path.Combine(root, "boot_a.img");
            File.WriteAllText(outputPath, "previous-good-backup");
            var plan = new PartitionExecutionPlan(
                "FAST123",
                PartitionTransportKind.Fastboot,
                PartitionOperationKind.Backup,
                [new PartitionTask("boot_a", "boot_a", null, outputPath, 64)]);

            // 设备返回残缺(1 字节)备份:完整性校验必须失败,绝不覆盖既有完好备份。
            await Assert.ThrowsAsync<InvalidOperationException>(() =>
                service.ExecuteAsync(plan, (_, _) => { }, _ => { }, CancellationToken.None));

            File.ReadAllText(outputPath).Should().Be("previous-good-backup");
        }
        finally
        {
            Directory.Delete(root, recursive: true);
        }
    }

    private static DeviceSessionViewModel CreateFastbootSession(string serial)
    {
        var session = new DeviceSessionViewModel();
        session.ApplyDevice(new DeviceSnapshot(DeviceConnectionState.FastbootConnected, serial, "Fastboot 已连接"));
        return session;
    }

    private sealed class RecordingPartitionTransport(string? failOn = null) : IPartitionTransport
    {
        public List<string> Writes { get; } = [];

        public bool TruncateBackup { get; set; }

        public PartitionTransportKind Kind => PartitionTransportKind.Fastboot;

        public Task<PartitionSnapshot> DiscoverAsync(string serial, CancellationToken cancellationToken) =>
            Task.FromResult(new PartitionSnapshot(serial, Kind, "a", []));

        public Task BackupAsync(string serial, PartitionTask task, IProgress<PartitionTransferProgress>? progress, CancellationToken cancellationToken)
        {
            if (TruncateBackup)
            {
                // 模拟设备返回残缺备份(远小于期望大小)。
                File.WriteAllBytes(task.OutputPath!, [0x01]);
                return Task.CompletedTask;
            }

            // 写入与 task.SizeBytes 一致的内容,让执行服务的备份完整性校验通过。
            var size = (int)(task.SizeBytes ?? (long)task.PartitionName.Length);
            File.WriteAllBytes(task.OutputPath!, new byte[size]);
            return Task.CompletedTask;
        }

        public Task WriteAsync(string serial, PartitionTask task, IProgress<PartitionTransferProgress>? progress, CancellationToken cancellationToken)
        {
            Writes.Add(task.PartitionName);
            if (task.PartitionName == failOn)
            {
                throw new PartitionOperationException(Kind, task.PartitionName, "写入", new InvalidOperationException("模拟失败"));
            }

            return Task.CompletedTask;
        }

        public Task EraseAsync(string serial, PartitionTask task, IProgress<PartitionTransferProgress>? progress, CancellationToken cancellationToken) =>
            Task.CompletedTask;
    }
}
