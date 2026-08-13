using VivoKsu.App.Models;
using VivoKsu.App.Services;
using VivoKsu.App.ViewModels;

namespace VivoKsu.App.Tests;

public class OperationCoordinatorTests
{
    [Fact]
    public async Task RunAsync_serializes_concurrent_operations_and_restores_idle_state()
    {
        var (coordinator, session, _) = CreateCoordinator();
        var firstEntered = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);
        var releaseFirst = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);
        var secondEntered = false;

        var first = coordinator.RunAsync(OperationKind.Flashing, "正在刷写 boot", async (_, token) =>
        {
            firstEntered.SetResult(true);
            await releaseFirst.Task.WaitAsync(token);
        });
        await firstEntered.Task.WaitAsync(TimeSpan.FromSeconds(2));

        var second = coordinator.RunAsync(OperationKind.Rebooting, "正在重启设备", (_, _) =>
        {
            secondEntered = true;
            return Task.CompletedTask;
        });

        Assert.True(coordinator.IsBusy);
        Assert.False(secondEntered);

        releaseFirst.SetResult(true);
        await Task.WhenAll(first, second);

        Assert.True(secondEntered);
        Assert.False(coordinator.IsBusy);
        Assert.Equal(OperationKind.Idle, coordinator.State.Kind);
        Assert.Equal(OperationKind.Completed, session.OperationKind);
    }

    [Fact]
    public async Task CancelCurrent_cancels_the_active_delegate_and_records_a_warning()
    {
        var (coordinator, session, logs) = CreateCoordinator();
        var entered = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);
        var operation = coordinator.RunAsync(OperationKind.Flashing, "正在刷写 boot", async (_, token) =>
        {
            entered.SetResult(true);
            await Task.Delay(Timeout.InfiniteTimeSpan, token);
        });
        await entered.Task.WaitAsync(TimeSpan.FromSeconds(2));

        coordinator.CancelCurrent();

        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => operation);
        Assert.False(coordinator.IsBusy);
        Assert.Equal(OperationKind.Canceled, session.OperationKind);
        Assert.Contains(logs.Entries, entry =>
            entry.Level == OperationLogLevel.Warning &&
            entry.OperationId is not null &&
            entry.Message.Contains("已取消", StringComparison.Ordinal));
    }

    [Fact]
    public async Task ReportStage_updates_current_state_and_writes_a_correlated_log_entry()
    {
        var (coordinator, session, logs) = CreateCoordinator();
        var stageReported = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);
        var releaseOperation = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);

        var operation = coordinator.RunAsync(OperationKind.Transferring, "正在传输文件", async (context, token) =>
        {
            context.ReportStage("正在上传 boot.img");
            context.ReportProgress(0.5);
            stageReported.SetResult(true);
            await releaseOperation.Task.WaitAsync(token);
        });

        await stageReported.Task.WaitAsync(TimeSpan.FromSeconds(2));

        Assert.True(coordinator.IsBusy);
        Assert.Equal(OperationKind.Transferring, coordinator.State.Kind);
        Assert.Equal("正在上传 boot.img", coordinator.State.Stage);
        Assert.Equal(0.5, coordinator.State.Progress);
        Assert.Equal("正在上传 boot.img", session.StatusText);
        Assert.Contains(logs.Entries, entry =>
            entry.Message == "正在上传 boot.img" &&
            entry.OperationId is not null);

        releaseOperation.SetResult(true);
        await operation;

        Assert.Equal(OperationKind.Completed, session.OperationKind);
        Assert.Equal("操作完成", session.StatusText);
    }

    [Fact]
    public async Task ReportStage_can_transition_the_active_operation_kind()
    {
        var (coordinator, session, _) = CreateCoordinator();
        OperationStateSnapshot? reported = null;
        OperationKind? reportedSessionKind = null;
        coordinator.StateChanged += (_, _) =>
        {
            if (coordinator.IsBusy && coordinator.State.Stage == "正在重启至 bootloader")
            {
                reported = coordinator.State;
                reportedSessionKind = session.OperationKind;
            }
        };

        await coordinator.RunAsync(OperationKind.Installing, "ROOT 自动流程", (context, _) =>
        {
            context.ReportStage("正在重启至 bootloader", OperationKind.Rebooting);
            return Task.CompletedTask;
        });

        Assert.NotNull(reported);
        Assert.Equal(OperationKind.Rebooting, reported!.Kind);
        Assert.Equal(OperationKind.Rebooting, reportedSessionKind);
    }

    [Fact]
    public async Task Dispose_cancels_an_active_operation_without_replacing_the_cancellation_exception()
    {
        var (coordinator, _, _) = CreateCoordinator();
        var entered = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);
        var operation = coordinator.RunAsync(OperationKind.Flashing, "正在刷写 boot", async (_, token) =>
        {
            entered.SetResult(true);
            await Task.Delay(Timeout.InfiniteTimeSpan, token);
        });
        await entered.Task.WaitAsync(TimeSpan.FromSeconds(2));

        coordinator.Dispose();

        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => operation);
    }

    [Fact]
    public async Task ReportProgress_does_not_write_a_log_entry_for_every_progress_update()
    {
        var (coordinator, session, logs) = CreateCoordinator();
        var progressReported = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);
        var releaseOperation = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);

        var operation = coordinator.RunAsync(OperationKind.Transferring, "正在传输文件", async (context, token) =>
        {
            context.ReportStage("正在上传 boot.img");
            var countBefore = logs.Entries.Count;
            for (var i = 0; i < 50; i++)
            {
                context.ReportProgress(i / 50d);
            }

            Assert.Equal(countBefore, logs.Entries.Count);
            Assert.Equal(0.98, coordinator.State.Progress ?? 0, precision: 3);
            progressReported.SetResult(true);
            await releaseOperation.Task.WaitAsync(token);
        });

        await progressReported.Task.WaitAsync(TimeSpan.FromSeconds(2));

        releaseOperation.SetResult(true);
        await operation;
    }

    private static (OperationCoordinator Coordinator, DeviceSessionViewModel Session, OperationLogService Logs) CreateCoordinator()
    {
        var session = new DeviceSessionViewModel();
        var logs = new OperationLogService();
        return (new OperationCoordinator(session, logs), session, logs);
    }
}
