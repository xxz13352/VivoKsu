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
        if (session.ConnectionState != DeviceConnectionState.AdbConnected)
        {
            logs.Write(OperationLogLevel.Warning, "ADB 设备未就绪，无法执行重启。");
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
                        await backend.RebootAsync(session.Serial, target, cancellationToken);
                    });
                return;
            }

            session.BeginOperation(OperationKind.Rebooting, status);
            logs.Write(OperationLogLevel.Info, status);
            await backend.RebootAsync(session.Serial, target, CancellationToken.None);
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
}
