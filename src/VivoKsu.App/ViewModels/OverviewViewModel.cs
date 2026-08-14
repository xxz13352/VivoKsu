using CommunityToolkit.Mvvm.Input;
using VivoKsu.App.Models;
using VivoKsu.App.Services;

namespace VivoKsu.App.ViewModels;

public sealed class OverviewViewModel
{
    private readonly DeviceSessionViewModel session;
    private readonly FastbootRsBackend backend;
    private readonly OperationLogService logs;
    private readonly IOperationCoordinator? coordinator;

    public OverviewViewModel(
        DeviceSessionViewModel session,
        FastbootRsBackend backend,
        OperationLogService logs,
        IOperationCoordinator? coordinator = null)
    {
        this.session = session;
        this.backend = backend;
        this.logs = logs;
        this.coordinator = coordinator;
        RebootSystemCommand = new AsyncRelayCommand(() => RebootAsync(string.Empty, "正在重启设备"));
        RebootBootloaderCommand = new AsyncRelayCommand(() => RebootAsync("bootloader", "正在重启至 Bootloader"));
        RebootFastbootCommand = new AsyncRelayCommand(() => RebootAsync("fastboot", "正在重启至 Fastboot"));
    }

    public IAsyncRelayCommand RebootSystemCommand { get; }
    public IAsyncRelayCommand RebootBootloaderCommand { get; }
    public IAsyncRelayCommand RebootFastbootCommand { get; }

    private async Task RebootAsync(string target, string status)
    {
        var state = session.ConnectionState;
        if (state is not (DeviceConnectionState.AdbConnected or DeviceConnectionState.FastbootConnected))
        {
            logs.Write(OperationLogLevel.Warning, "设备未就绪(需要 ADB 或 Fastboot 连接)，无法执行重启。");
            return;
        }

        try
        {
            if (coordinator is not null)
            {
                await coordinator.RunAsync(
                    OperationKind.Rebooting,
                    status,
                    async (context, cancellationToken) =>
                    {
                        context.ReportStage(status);
                        await RebootWithDispatchAsync(target, state, cancellationToken);
                    });
                return;
            }

            session.BeginOperation(OperationKind.Rebooting, status);
            logs.Write(OperationLogLevel.Info, status);
            await RebootWithDispatchAsync(target, state, CancellationToken.None);
            session.CompleteOperation("重启指令已发送");
            logs.Write(OperationLogLevel.Success, "重启指令已发送。");
        }
        catch (OperationCanceledException)
        {
        }
        catch (Exception exception)
        {
            if (coordinator is null)
            {
                session.FailOperation("重启失败");
                logs.Write(OperationLogLevel.Error, exception.Message);
            }
        }
    }

    /// <summary>
    /// 按当前会话模式选择重启通道:ADB 用 <c>adb reboot</c>,fastboot / fastbootd
    /// 用 <c>fastboot reboot</c>(fastboot 串号对 adb 不可见)。三种目标在两种模式下都可用:
    /// 空=回系统、bootloader=引导加载器、fastboot=fastbootd。
    /// </summary>
    private async Task RebootWithDispatchAsync(string target, DeviceConnectionState state, CancellationToken cancellationToken)
    {
        if (state == DeviceConnectionState.FastbootConnected)
        {
            await backend.FastbootRebootAsync(session.Serial, target, cancellationToken);
            return;
        }

        await backend.RebootAsync(session.Serial, target, cancellationToken);
    }
}
