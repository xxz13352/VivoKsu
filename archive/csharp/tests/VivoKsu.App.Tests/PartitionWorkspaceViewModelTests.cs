using FluentAssertions;
using VivoKsu.App.Models;
using VivoKsu.App.Services;
using VivoKsu.App.ViewModels;

namespace VivoKsu.App.Tests;

public class PartitionWorkspaceViewModelTests
{
    [Fact]
    public async Task MapImages_assigns_a_slotless_image_to_the_active_slot_without_rejecting_its_filename()
    {
        var session = CreateFastbootSession();
        var viewModel = CreateWorkspace(session);

        await viewModel.RefreshAsync(logIfUnavailable: true);
        viewModel.MapImages([new FlashImageInfo(@"D:\firmware\boot.img", 1024)]);

        viewModel.Rows.Single(row => row.Name == "boot_b").ImagePath.Should().Be(@"D:\firmware\boot.img");
    }

    [Fact]
    public async Task Refresh_keeps_the_existing_table_when_the_device_is_temporarily_unavailable()
    {
        var session = CreateFastbootSession();
        var viewModel = CreateWorkspace(session);
        await viewModel.RefreshAsync(logIfUnavailable: true);

        session.ApplyDevice(new DeviceSnapshot(DeviceConnectionState.Disconnected, "--", "等待连接"));
        await viewModel.RefreshAsync(logIfUnavailable: false);

        viewModel.Rows.Select(row => row.Name).Should().Contain(["boot_a", "boot_b", "super"]);
    }

    [Fact]
    public async Task Refresh_keeps_the_checkbox_selection_on_the_matching_partition()
    {
        var session = CreateFastbootSession();
        var viewModel = CreateWorkspace(session);
        await viewModel.RefreshAsync(logIfUnavailable: true);

        viewModel.Rows.Single(row => row.Name == "boot_a").IsSelected = true;

        await viewModel.RefreshAsync(logIfUnavailable: false);

        viewModel.Rows.Single(row => row.Name == "boot_a").IsSelected.Should().BeTrue();
        viewModel.Rows.Single(row => row.Name == "boot_b").IsSelected.Should().BeFalse();
    }

    [Fact]
    public async Task Refresh_keeps_the_mapped_image_on_the_matching_partition()
    {
        var session = CreateFastbootSession();
        var viewModel = CreateWorkspace(session);
        await viewModel.RefreshAsync(logIfUnavailable: true);

        viewModel.Rows.Single(row => row.Name == "boot_a").ImagePath = @"D:\firmware\boot.img";

        await viewModel.RefreshAsync(logIfUnavailable: false);

        viewModel.Rows.Single(row => row.Name == "boot_a").ImagePath.Should().Be(@"D:\firmware\boot.img");
        viewModel.Rows.Single(row => row.Name == "boot_b").HasImage.Should().BeFalse();
    }

    [Fact]
    public async Task Write_updates_overall_progress_speed_and_elapsed()
    {
        var session = CreateFastbootSession();
        var viewModel = CreateWorkspace(session, new BlockingReportingTransport(CreateSnapshot()));
        await viewModel.RefreshAsync(logIfUnavailable: true);

        viewModel.Rows.Single(row => row.Name == "boot_a").IsSelected = true;
        viewModel.Rows.Single(row => row.Name == "boot_b").IsSelected = true;
        viewModel.Rows.Single(row => row.Name == "boot_a").ImagePath = @"D:\firmware\boot_a.img";
        viewModel.Rows.Single(row => row.Name == "boot_b").ImagePath = @"D:\firmware\boot_b.img";

        await viewModel.WriteSelectedCommand.ExecuteAsync(null);

        viewModel.OverallProgress.Should().Be(1.0);
        viewModel.OperationSpeedText.Should().NotBe("--");
        viewModel.OperationElapsedText.Should().NotBe("00:00");
        viewModel.ProgressText.Should().Contain("已完成 2 个分区");
    }

    [Fact]
    public async Task Write_tracks_the_current_partition_progress_mid_operation()
    {
        var session = CreateFastbootSession();
        var transport = new BlockingReportingTransport(CreateSnapshot());
        var viewModel = CreateWorkspace(session, transport);
        await viewModel.RefreshAsync(logIfUnavailable: true);

        viewModel.Rows.Single(row => row.Name == "boot_a").IsSelected = true;
        viewModel.Rows.Single(row => row.Name == "boot_a").ImagePath = @"D:\firmware\boot_a.img";
        var gate = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);
        transport.Gate = gate;

        var operation = viewModel.WriteSelectedCommand.ExecuteAsync(null);
        await WaitUntilAsync(() =>
            viewModel.CurrentOperationPartitionName == "boot_a" && viewModel.CurrentOperationProgress > 0);

        viewModel.CurrentOperationPartitionName.Should().Be("boot_a");
        viewModel.CurrentOperationProgress.Should().Be(0.5);
        viewModel.OperationSpeedText.Should().NotBe("--");

        gate.SetResult(true);
        await operation;
        await WaitUntilAsync(() => viewModel.ProgressText.Contains("已完成"));
        viewModel.CurrentOperationPartitionName.Should().Be("--");
    }

    private static PartitionSnapshot CreateSnapshot() => new(
        "FAST123",
        PartitionTransportKind.Fastboot,
        "b",
        [
            new DevicePartition("boot_a", "boot_a", 64, "a", false, false, true),
            new DevicePartition("boot_b", "boot_b", 64, "b", false, false, true),
            new DevicePartition("super", "super", 8_000, string.Empty, false, true, true)
        ]);

    private static PartitionWorkspaceViewModel CreateWorkspace(DeviceSessionViewModel session) =>
        CreateWorkspace(session, new FixedPartitionTransport(PartitionTransportKind.Fastboot, CreateSnapshot()));

    private static PartitionWorkspaceViewModel CreateWorkspace(
        DeviceSessionViewModel session,
        IPartitionTransport fastboot)
    {
        var logs = new OperationLogService();
        var coordinator = new OperationCoordinator(session, logs);
        var adbRoot = new FixedPartitionTransport(
            PartitionTransportKind.AdbRoot,
            new PartitionSnapshot("FAST123", PartitionTransportKind.AdbRoot, "b", []));
        var executor = new PartitionExecutionService(session, coordinator, logs, [fastboot, adbRoot]);
        return new PartitionWorkspaceViewModel(session, fastboot, adbRoot, executor, logs, coordinator, _ => true);
    }

    private static DeviceSessionViewModel CreateFastbootSession()
    {
        var session = new DeviceSessionViewModel();
        session.ApplyDevice(new DeviceSnapshot(DeviceConnectionState.FastbootConnected, "FAST123", "Fastboot 已连接"));
        return session;
    }

    private static async Task WaitUntilAsync(Func<bool> predicate)
    {
        var deadline = DateTimeOffset.UtcNow.AddSeconds(2);
        while (!predicate())
        {
            if (DateTimeOffset.UtcNow >= deadline)
            {
                throw new TimeoutException("等待分区操作状态超时。 ");
            }

            await Task.Delay(10);
        }
    }

    private sealed class FixedPartitionTransport(PartitionTransportKind kind, PartitionSnapshot snapshot) : IPartitionTransport
    {
        public PartitionTransportKind Kind => kind;

        public Task<PartitionSnapshot> DiscoverAsync(string serial, CancellationToken cancellationToken) => Task.FromResult(snapshot);
        public Task BackupAsync(string serial, PartitionTask task, IProgress<PartitionTransferProgress>? progress, CancellationToken cancellationToken) => Task.CompletedTask;
        public Task WriteAsync(string serial, PartitionTask task, IProgress<PartitionTransferProgress>? progress, CancellationToken cancellationToken) => Task.CompletedTask;
        public Task EraseAsync(string serial, PartitionTask task, IProgress<PartitionTransferProgress>? progress, CancellationToken cancellationToken) => Task.CompletedTask;
    }

    /// <summary>Reports a 50% progress report, then blocks on <see cref="Gate"/> before completing.</summary>
    private sealed class BlockingReportingTransport(PartitionSnapshot snapshot) : IPartitionTransport
    {
        public PartitionTransportKind Kind => PartitionTransportKind.Fastboot;

        public TaskCompletionSource<bool>? Gate { get; set; }

        public Task<PartitionSnapshot> DiscoverAsync(string serial, CancellationToken cancellationToken) => Task.FromResult(snapshot);
        public Task BackupAsync(string serial, PartitionTask task, IProgress<PartitionTransferProgress>? progress, CancellationToken cancellationToken) => Task.CompletedTask;
        public Task EraseAsync(string serial, PartitionTask task, IProgress<PartitionTransferProgress>? progress, CancellationToken cancellationToken) => Task.CompletedTask;

        public async Task WriteAsync(string serial, PartitionTask task, IProgress<PartitionTransferProgress>? progress, CancellationToken cancellationToken)
        {
            var size = task.SizeBytes.GetValueOrDefault();
            progress?.Report(new PartitionTransferProgress(task.PartitionName, size / 2, task.SizeBytes, 5_000_000));
            if (Gate is not null)
            {
                await Gate.Task.WaitAsync(cancellationToken);
            }

            progress?.Report(new PartitionTransferProgress(task.PartitionName, size, task.SizeBytes, 5_000_000));
        }
    }
}
