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
        var service = new DeviceSessionService(backend, new DeviceInfoService(backend, new FakeFastbootCliRunner()), new OperationLogService());
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

    [Fact]
    public async Task LogoutCommand_invokes_the_injected_logout_callback()
    {
        var invoked = false;
        var viewModel = new MainViewModel(new DeviceSessionViewModel(), onLogout: () => { invoked = true; return Task.CompletedTask; });

        await viewModel.LogoutCommand.ExecuteAsync(null);

        Assert.True(invoked);
    }

    [Fact]
    public async Task LogoutCommand_is_disabled_while_the_coordinator_is_busy()
    {
        var session = new DeviceSessionViewModel();
        var coordinator = new OperationCoordinator(session, new OperationLogService());
        var viewModel = new MainViewModel(session, coordinator: coordinator);
        using var started = new ManualResetEventSlim();
        using var release = new ManualResetEventSlim();
        // RunAsync 会同步进入操作体直到首个真正的挂起点,而操作体会阻塞等待 release;
        // 需在后台线程跑协调器,让断言线程得以检查命令可用性。
        var run = Task.Run(async () =>
            await coordinator.RunAsync(OperationKind.Flashing, "test", async (_, _) =>
            {
                started.Set();
                // 大余量(30s):本机调度受并行会话/CI 负载影响时 5s 可能不够,避免定时偶发。
                release.Wait(TimeSpan.FromSeconds(30));
                await Task.CompletedTask;
            }));

        Assert.True(started.Wait(TimeSpan.FromSeconds(30)));
        // CanExecute 同步读 coordinator.IsBusy(SetCurrent 先于操作体执行,已置忙)。
        Assert.True(coordinator.IsBusy);
        Assert.False(viewModel.LogoutCommand.CanExecute(null));

        release.Set();
        await run;

        Assert.True(viewModel.LogoutCommand.CanExecute(null));
    }

    [Fact]
    public void LogoutCommand_is_disabled_while_the_session_is_busy_even_without_a_coordinator()
    {
        var session = new DeviceSessionViewModel();
        var viewModel = new MainViewModel(session);

        Assert.True(viewModel.LogoutCommand.CanExecute(null));

        // 不走协调器的页面(LineFlash 历史遗留)直接置会话忙 → 登出也应禁用(DeviceSession.IsBusy 兜底)。
        session.BeginOperation(OperationKind.Flashing, "正在执行");
        Assert.False(viewModel.LogoutCommand.CanExecute(null));

        session.CompleteOperation("完成");
        Assert.True(viewModel.LogoutCommand.CanExecute(null));
    }

    [Fact]
    public void AccountName_is_settable()
    {
        var viewModel = new MainViewModel(new DeviceSessionViewModel());

        viewModel.AccountName = "alice";

        Assert.Equal("alice", viewModel.AccountName);
    }

    private sealed class CurrentDeviceApi : IFastbootRsNativeApi
    {
        public string ListDevices() => "AUTO\tdevice\n";
        public string Shell(string? serial, string command, int timeoutMilliseconds = 15000) => command == "getprop"
            ? "[ro.product.model]: [自动检测设备]\n"
            : string.Empty;
        public string GetVar(string? serial, string variable) => string.Empty;
        public void Reboot(string? serial, string target) { }
        public void Push(string? serial, string localPath, string remotePath, int timeoutMilliseconds = 15000) { }
        public long Pull(string? serial, string remotePath, string localPath, int timeoutMilliseconds = 15000) => 0;
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
