using VivoKsu.App.Models;
using VivoKsu.App.Services;
using VivoKsu.App.ViewModels;

namespace VivoKsu.App.Tests;

public class DeviceMonitorServiceTests
{
    [Fact]
    public async Task RefreshAutomaticallyAsync_skips_discovery_while_a_foreground_operation_is_running()
    {
        var (coordinator, session, _) = CreateCoordinator();
        var refresh = new RecordingRefreshService();
        var monitor = new DeviceMonitorService(refresh, session, coordinator, TimeSpan.FromHours(1));
        var release = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);

        var operation = coordinator.RunAsync(
            OperationKind.Flashing,
            "正在刷写 boot",
            (_, token) => release.Task.WaitAsync(token));
        await WaitUntilAsync(() => coordinator.IsBusy);

        await monitor.RefreshAutomaticallyAsync();

        Assert.Equal(0, refresh.CallCount);

        release.SetResult(true);
        await operation;
        await monitor.StopAsync();
    }

    [Fact]
    public async Task Completing_a_foreground_operation_triggers_one_compensating_automatic_refresh()
    {
        var (coordinator, session, _) = CreateCoordinator();
        var refresh = new RecordingRefreshService();
        var monitor = new DeviceMonitorService(refresh, session, coordinator, TimeSpan.FromHours(1));

        await coordinator.RunAsync(OperationKind.Rebooting, "正在重启设备", (_, _) => Task.CompletedTask);
        await WaitUntilAsync(() => refresh.CallCount == 1);

        Assert.Equal([DeviceRefreshMode.Automatic], refresh.Modes);
        await monitor.StopAsync();
    }

    [Fact]
    public async Task RefreshManualAsync_and_automatic_refresh_share_one_refresh_gate()
    {
        var (coordinator, session, _) = CreateCoordinator();
        var refresh = new BlockingRefreshService();
        var monitor = new DeviceMonitorService(refresh, session, coordinator, TimeSpan.FromHours(1));

        var manual = monitor.RefreshManualAsync(logActivity: true);
        await refresh.Entered.Task.WaitAsync(TimeSpan.FromSeconds(2));

        await monitor.RefreshAutomaticallyAsync();

        Assert.Equal(1, refresh.CallCount);

        refresh.Release.SetResult(true);
        await manual;
        await monitor.StopAsync();
    }

    [Fact]
    public async Task Heartbeat_fires_device_refreshed_only_when_the_device_identity_changes()
    {
        var (coordinator, session, _) = CreateCoordinator();
        var monitor = new DeviceMonitorService(
            new ApplyingRefreshService(), session, coordinator, TimeSpan.FromHours(1));
        var fired = 0;
        monitor.DeviceRefreshed += (_, _) =>
        {
            fired++;
            return Task.CompletedTask;
        };

        // First heartbeat: disconnected -> ADB, so downstream handlers must fire.
        await monitor.RefreshHeartbeatAsync();
        Assert.Equal(1, fired);

        // Second heartbeat: identity unchanged, the partition table must NOT be re-read.
        await monitor.RefreshHeartbeatAsync();
        Assert.Equal(1, fired);

        await monitor.StopAsync();
    }

    [Fact]
    public async Task Heartbeat_fires_again_after_the_device_disconnects()
    {
        var (coordinator, session, _) = CreateCoordinator();
        var applied = new ApplyingRefreshService();
        var monitor = new DeviceMonitorService(applied, session, coordinator, TimeSpan.FromHours(1));
        var fired = 0;
        monitor.DeviceRefreshed += (_, _) =>
        {
            fired++;
            return Task.CompletedTask;
        };

        await monitor.RefreshHeartbeatAsync();
        Assert.Equal(1, fired);

        applied.Apply(DeviceConnectionState.Disconnected, "--");
        await monitor.RefreshHeartbeatAsync();
        Assert.Equal(2, fired);

        await monitor.StopAsync();
    }

    [Fact]
    public async Task Explicit_automatic_refresh_fires_even_when_the_identity_is_unchanged()
    {
        var (coordinator, session, _) = CreateCoordinator();
        var monitor = new DeviceMonitorService(
            new ApplyingRefreshService(), session, coordinator, TimeSpan.FromHours(1));
        var fired = 0;
        monitor.DeviceRefreshed += (_, _) =>
        {
            fired++;
            return Task.CompletedTask;
        };

        await monitor.RefreshAutomaticallyAsync();
        await monitor.RefreshAutomaticallyAsync();

        Assert.Equal(2, fired);
        await monitor.StopAsync();
    }

    private static (OperationCoordinator Coordinator, DeviceSessionViewModel Session, OperationLogService Logs) CreateCoordinator()
    {
        var session = new DeviceSessionViewModel();
        var logs = new OperationLogService();
        return (new OperationCoordinator(session, logs), session, logs);
    }

    private static async Task WaitUntilAsync(Func<bool> predicate)
    {
        var deadline = DateTimeOffset.UtcNow.AddSeconds(2);
        while (!predicate())
        {
            if (DateTimeOffset.UtcNow >= deadline)
            {
                throw new TimeoutException("等待异步设备监视状态超时。 ");
            }

            await Task.Delay(10);
        }
    }

    private sealed class RecordingRefreshService : IDeviceRefreshService
    {
        public int CallCount { get; private set; }

        public List<DeviceRefreshMode> Modes { get; } = [];

        public Task RefreshAsync(
            DeviceSessionViewModel session,
            CancellationToken cancellationToken,
            bool logActivity = true,
            DeviceRefreshMode refreshMode = DeviceRefreshMode.Manual)
        {
            cancellationToken.ThrowIfCancellationRequested();
            CallCount++;
            Modes.Add(refreshMode);
            return Task.CompletedTask;
        }
    }

    private sealed class BlockingRefreshService : IDeviceRefreshService
    {
        public TaskCompletionSource<bool> Entered { get; } = new(TaskCreationOptions.RunContinuationsAsynchronously);

        public TaskCompletionSource<bool> Release { get; } = new(TaskCreationOptions.RunContinuationsAsynchronously);

        public int CallCount { get; private set; }

        public async Task RefreshAsync(
            DeviceSessionViewModel session,
            CancellationToken cancellationToken,
            bool logActivity = true,
            DeviceRefreshMode refreshMode = DeviceRefreshMode.Manual)
        {
            CallCount++;
            Entered.SetResult(true);
            await Release.Task.WaitAsync(cancellationToken);
        }
    }

    /// <summary>Applies a connectable device to the session on every refresh, so the monitor's
    /// identity-change gating can be observed.</summary>
    private sealed class ApplyingRefreshService : IDeviceRefreshService
    {
        private DeviceConnectionState state = DeviceConnectionState.AdbConnected;

        private string serial = "b8d2de49";

        public void Apply(DeviceConnectionState state, string serial)
        {
            this.state = state;
            this.serial = serial;
        }

        public Task RefreshAsync(
            DeviceSessionViewModel session,
            CancellationToken cancellationToken,
            bool logActivity = true,
            DeviceRefreshMode refreshMode = DeviceRefreshMode.Manual)
        {
            session.ApplyDevice(new DeviceSnapshot(state, serial, state == DeviceConnectionState.AdbConnected ? "ADB 连接" : "等待连接"));
            return Task.CompletedTask;
        }
    }
}
