using System.Threading;
using VivoKsu.App.Models;
using VivoKsu.App.ViewModels;

namespace VivoKsu.App.Services;

public sealed class DeviceMonitorService : IDeviceMonitor, IAsyncDisposable
{
    private readonly IDeviceRefreshService refreshService;
    private readonly DeviceSessionViewModel session;
    private readonly IOperationCoordinator coordinator;
    private readonly OperationLogService logs;
    private readonly TimeSpan interval;
    private readonly SynchronizationContext? synchronizationContext;
    private readonly SemaphoreSlim refreshGate = new(1, 1);
    private CancellationTokenSource? lifecycleCancellation;
    private Task? monitorTask;
    private int compensatingRefreshScheduled;
    private int activeRefreshes;
    private readonly object drainLock = new();
    private TaskCompletionSource? refreshDrain;
    private bool stopped;

    public DeviceMonitorService(
        IDeviceRefreshService refreshService,
        DeviceSessionViewModel session,
        IOperationCoordinator coordinator,
        TimeSpan? interval = null,
        SynchronizationContext? synchronizationContext = null,
        OperationLogService? logs = null)
    {
        this.refreshService = refreshService;
        this.session = session;
        this.coordinator = coordinator;
        this.interval = interval ?? TimeSpan.FromSeconds(3);
        this.synchronizationContext = synchronizationContext ?? SynchronizationContext.Current;
        this.logs = logs ?? new OperationLogService();
        coordinator.StateChanged += OnCoordinatorStateChanged;
    }

    public event Func<DeviceRefreshMode, CancellationToken, Task>? DeviceRefreshed;

    public Task StartAsync(CancellationToken cancellationToken = default)
    {
        if (stopped)
        {
            throw new ObjectDisposedException(nameof(DeviceMonitorService));
        }

        if (monitorTask is not null)
        {
            return Task.CompletedTask;
        }

        lifecycleCancellation = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        monitorTask = MonitorLoopAsync(lifecycleCancellation.Token);
        return Task.CompletedTask;
    }

    public async Task StopAsync()
    {
        if (stopped)
        {
            return;
        }

        stopped = true;
        coordinator.StateChanged -= OnCoordinatorStateChanged;
        lifecycleCancellation?.Cancel();

        var activeTask = monitorTask;
        if (activeTask is not null)
        {
            try
            {
                await activeTask;
            }
            catch (OperationCanceledException) when (lifecycleCancellation?.IsCancellationRequested == true)
            {
            }
        }

        // Wait (bounded) for any compensating/manual refresh kicked off just before
        // shutdown to finish using the gate before it is disposed.
        try
        {
            await DrainRefreshesAsync().WaitAsync(TimeSpan.FromSeconds(5));
        }
        catch (Exception)
        {
            // A stuck refresh should not block shutdown.
        }

        lifecycleCancellation?.Dispose();
        lifecycleCancellation = null;
        monitorTask = null;
    }

    public Task RefreshManualAsync(bool logActivity, CancellationToken cancellationToken = default) =>
        RefreshCoreAsync(logActivity, DeviceRefreshMode.Manual, cancellationToken, forceFire: true);

    public Task RefreshAutomaticallyAsync(CancellationToken cancellationToken = default) =>
        RefreshCoreAsync(logActivity: false, DeviceRefreshMode.Automatic, cancellationToken, forceFire: true);

    /// <summary>
    /// Periodic heartbeat poll: still refreshes the device session (battery, model, …) but only
    /// fires the downstream DeviceRefreshed handlers when the device identity actually changed,
    /// so the partition table is not re-read every tick.
    /// </summary>
    public Task RefreshHeartbeatAsync(CancellationToken cancellationToken = default) =>
        RefreshCoreAsync(logActivity: false, DeviceRefreshMode.Automatic, cancellationToken, forceFire: false);

    public async ValueTask DisposeAsync()
    {
        await StopAsync();
        refreshGate.Dispose();
    }

    private async Task MonitorLoopAsync(CancellationToken cancellationToken)
    {
        using var timer = new PeriodicTimer(interval);
        try
        {
            while (await timer.WaitForNextTickAsync(cancellationToken))
            {
                try
                {
                    await RefreshHeartbeatAsync(cancellationToken);
                }
                catch (OperationCanceledException) when (cancellationToken.IsCancellationRequested)
                {
                    throw;
                }
                catch (Exception exception)
                {
                    // One failed tick must not terminate automatic detection permanently.
                    logs.Write(OperationLogLevel.Error, $"设备自动检测失败（跳过本次）: {exception.Message}");
                }
            }
        }
        catch (OperationCanceledException) when (cancellationToken.IsCancellationRequested)
        {
        }
        catch (Exception exception)
        {
            logs.Write(OperationLogLevel.Error, $"设备自动检测循环失败: {exception.Message}");
        }
    }

    private async Task RefreshCoreAsync(
        bool logActivity,
        DeviceRefreshMode refreshMode,
        CancellationToken cancellationToken,
        bool forceFire = false)
    {
        BeginRefresh();
        try
        {
            if (stopped || coordinator.IsBusy)
            {
                return;
            }

            if (!await refreshGate.WaitAsync(0, cancellationToken))
            {
                return;
            }

            try
            {
                if (coordinator.IsBusy)
                {
                    return;
                }

                var identityBefore = GetSessionIdentity();
                await RunOnSynchronizationContextAsync(
                    token => refreshService.RefreshAsync(session, token, logActivity, refreshMode),
                    cancellationToken);

                // The heartbeat poll runs every few seconds; re-reading the partition table on
                // every tick keeps the table busy forever. Only fire the downstream handlers when
                // the device actually changed, or when the caller asked explicitly (manual
                // refresh, or the compensating refresh after a flashing operation).
                var identityChanged = GetSessionIdentity() != identityBefore;
                if (!forceFire && refreshMode != DeviceRefreshMode.Manual && !identityChanged)
                {
                    return;
                }

                var handlers = DeviceRefreshed?.GetInvocationList();
                if (handlers is not null)
                {
                    foreach (var handler in handlers.Cast<Func<DeviceRefreshMode, CancellationToken, Task>>())
                    {
                        await handler(refreshMode, cancellationToken);
                    }
                }
            }
            finally
            {
                refreshGate.Release();
            }
        }
        finally
        {
            EndRefresh();
        }
    }

    private (DeviceConnectionState State, string Serial) GetSessionIdentity() =>
        (session.ConnectionState, session.Serial);

    private void OnCoordinatorStateChanged(object? sender, EventArgs eventArgs)
    {
        if (coordinator.IsBusy || Interlocked.Exchange(ref compensatingRefreshScheduled, 1) != 0)
        {
            return;
        }

        _ = RunCompensatingRefreshAsync(lifecycleCancellation?.Token ?? default);
    }

    private async Task RunCompensatingRefreshAsync(CancellationToken cancellationToken)
    {
        try
        {
            await RefreshAutomaticallyAsync(cancellationToken);
        }
        catch (OperationCanceledException)
        {
        }
        catch (Exception exception)
        {
            logs.Write(OperationLogLevel.Error, $"设备补偿检测失败: {exception.Message}");
        }
        finally
        {
            Volatile.Write(ref compensatingRefreshScheduled, 0);
        }
    }

    private void BeginRefresh()
    {
        lock (drainLock)
        {
            activeRefreshes++;
        }
    }

    private void EndRefresh()
    {
        TaskCompletionSource? drain = null;
        lock (drainLock)
        {
            activeRefreshes--;
            if (activeRefreshes == 0 && refreshDrain is not null)
            {
                drain = refreshDrain;
                refreshDrain = null;
            }
        }

        drain?.TrySetResult();
    }

    private Task DrainRefreshesAsync()
    {
        lock (drainLock)
        {
            if (activeRefreshes == 0)
            {
                return Task.CompletedTask;
            }

            refreshDrain ??= new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
            return refreshDrain.Task;
        }
    }

    private Task RunOnSynchronizationContextAsync(
        Func<CancellationToken, Task> operation,
        CancellationToken cancellationToken)
    {
        if (synchronizationContext is null || SynchronizationContext.Current == synchronizationContext)
        {
            return operation(cancellationToken);
        }

        var completion = new TaskCompletionSource<object?>(TaskCreationOptions.RunContinuationsAsynchronously);
        synchronizationContext.Post(async _ =>
        {
            try
            {
                await operation(cancellationToken);
                completion.SetResult(null);
            }
            catch (OperationCanceledException exception)
            {
                completion.SetCanceled(exception.CancellationToken);
            }
            catch (Exception exception)
            {
                completion.SetException(exception);
            }
        }, null);
        return completion.Task;
    }
}
