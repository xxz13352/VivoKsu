using VivoKsu.App.Models;
using VivoKsu.App.Services;
using VivoKsu.App.ViewModels;

namespace VivoKsu.App.Tests;

public class MainViewModelTests
{
    [Fact]
    public void Selecting_a_page_updates_the_current_page()
    {
        var viewModel = new MainViewModel(new DeviceSessionViewModel());

        viewModel.SelectPageCommand.Execute(AppPage.FileTransfer);

        Assert.Equal(AppPage.FileTransfer, viewModel.SelectedPage);
    }

    [Fact]
    public async Task RefreshDeviceAsync_updates_the_session_without_user_activity_logging()
    {
        var session = new DeviceSessionViewModel();
        var backend = new FastbootRsBackend(new CurrentDeviceApi());
        var service = new DeviceSessionService(backend, new DeviceInfoService(backend), new OperationLogService());
        var viewModel = new MainViewModel(session, deviceSessionService: service);

        await viewModel.RefreshDeviceAsync(logActivity: false);

        Assert.Equal(DeviceConnectionState.AdbConnected, session.ConnectionState);
        Assert.Equal("AUTO", session.Serial);
    }

    [Fact]
    public async Task RefreshDeviceAsync_delegates_to_the_monitor_when_one_is_supplied()
    {
        var session = new DeviceSessionViewModel();
        var monitor = new RecordingDeviceMonitor();
        var viewModel = new MainViewModel(session, deviceMonitor: monitor);

        await viewModel.RefreshDeviceAsync(logActivity: true);

        Assert.Equal(1, monitor.ManualRefreshCount);
    }

    [Fact]
    public async Task OnDeviceRefreshedAsync_does_not_read_the_partition_table()
    {
        var session = new DeviceSessionViewModel();
        session.ApplyDevice(new DeviceSnapshot(DeviceConnectionState.FastbootConnected, "FAST123", "Fastboot 已连接"));
        var logs = new OperationLogService();
        var coordinator = new OperationCoordinator(session, logs);
        var fastboot = new FixedPartitionTransport(
            PartitionTransportKind.Fastboot,
            new PartitionSnapshot("FAST123", PartitionTransportKind.Fastboot, "a", [
                new DevicePartition("boot_a", "boot_a", 64, "a", false, false, true)
            ]));
        var adbRoot = new FixedPartitionTransport(
            PartitionTransportKind.AdbRoot,
            new PartitionSnapshot("FAST123", PartitionTransportKind.AdbRoot, "a", []));
        var workspace = new PartitionWorkspaceViewModel(
            session,
            fastboot,
            adbRoot,
            new PartitionExecutionService(session, coordinator, logs, [fastboot, adbRoot]),
            logs,
            coordinator,
            _ => true);
        var viewModel = new MainViewModel(session, partitionWorkspace: workspace, coordinator: coordinator);

        // The visual partition table is only read when the user clicks 读取分区表;
        // device heartbeats / compensating refreshes must not re-read it.
        await viewModel.OnDeviceRefreshedAsync(DeviceRefreshMode.Automatic, CancellationToken.None);

        Assert.Empty(viewModel.PartitionWorkspace.Rows);
    }

    private sealed class CurrentDeviceApi : IFastbootRsNativeApi
    {
        public string ListDevices() => "AUTO\tdevice\n";
        public string Shell(string? serial, string command) => command == "getprop"
            ? "[ro.product.model]: [自动检测设备]\n"
            : string.Empty;
        public string GetVar(string? serial, string variable) => string.Empty;
        public void Reboot(string? serial, string target) { }
        public void Push(string? serial, string localPath, string remotePath) { }
        public long Pull(string? serial, string remotePath, string localPath) => 0;
        public string Install(string? serial, string apkPath, bool replace) => string.Empty;
        public void Flash(string? serial, string partition, string imagePath) { }
    }

    private sealed class RecordingDeviceMonitor : IDeviceMonitor
    {
        public int ManualRefreshCount { get; private set; }

        public Task StartAsync(CancellationToken cancellationToken = default) => Task.CompletedTask;

        public Task StopAsync() => Task.CompletedTask;

        public Task RefreshManualAsync(bool logActivity, CancellationToken cancellationToken = default)
        {
            ManualRefreshCount++;
            return Task.CompletedTask;
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
}
