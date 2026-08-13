using VivoKsu.App.Models;
using VivoKsu.App.Services;
using VivoKsu.App.ViewModels;

namespace VivoKsu.App.Tests;

public class OverviewViewModelTests
{
    [Fact]
    public async Task RebootBootloaderCommand_forwards_bootloader_target_and_logs_success()
    {
        var session = new DeviceSessionViewModel();
        session.ApplyDevice(new DeviceSnapshot(DeviceConnectionState.AdbConnected, "RF8", "ADB 已连接"));
        var native = new RebootNativeApi();
        var logs = new OperationLogService();
        var viewModel = new OverviewViewModel(session, new FastbootRsBackend(native), logs);

        await viewModel.RebootBootloaderCommand.ExecuteAsync(null);

        Assert.Equal(("RF8", "bootloader"), native.LastRebootRequest);
        Assert.Equal(OperationKind.Completed, session.OperationKind);
        Assert.Contains(logs.Entries, entry => entry.Level == OperationLogLevel.Success);
    }

    [Fact]
    public async Task RebootBootloaderCommand_uses_the_shared_coordinator_lifecycle()
    {
        var session = new DeviceSessionViewModel();
        session.ApplyDevice(new DeviceSnapshot(DeviceConnectionState.AdbConnected, "RF8", "ADB 已连接"));
        var logs = new OperationLogService();
        var coordinator = new OperationCoordinator(session, logs);
        var native = new RebootNativeApi();
        var viewModel = new OverviewViewModel(session, new FastbootRsBackend(native), logs, coordinator);

        await viewModel.RebootBootloaderCommand.ExecuteAsync(null);

        Assert.Equal(("RF8", "bootloader"), native.LastRebootRequest);
        Assert.False(coordinator.IsBusy);
        Assert.Contains(logs.Entries, entry => entry.OperationId is not null && entry.Level == OperationLogLevel.Success);
    }

    private sealed class RebootNativeApi : IFastbootRsNativeApi
    {
        public (string? Serial, string Target)? LastRebootRequest { get; private set; }
        public string ListDevices() => string.Empty;
        public string Shell(string? serial, string command) => string.Empty;
        public string GetVar(string? serial, string variable) => string.Empty;
        public void Reboot(string? serial, string target) => LastRebootRequest = (serial, target);
        public void Push(string? serial, string localPath, string remotePath) { }
        public long Pull(string? serial, string remotePath, string localPath) => 0;
        public string Install(string? serial, string apkPath, bool replace) => string.Empty;
        public void Flash(string? serial, string partition, string imagePath) { }
    }
}
