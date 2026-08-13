using VivoKsu.App.Models;
using VivoKsu.App.ViewModels;

namespace VivoKsu.App.Services;

public sealed class MirrorService
{
    private readonly IProcessRunner processRunner;
    private readonly OperationLogService logs;
    private readonly IScrcpyToolLocator scrcpyToolLocator;
    private readonly IScrcpyProvisioningService? provisioner;
    private readonly SynchronizationContext? synchronizationContext;
    private const int MaxConsecutiveRestartFailures = 3;
    private IRunningProcess? process;
    private DeviceSessionViewModel? trackedSession;
    private bool deliberateStop;
    private int consecutiveRestartFailures;

    public MirrorService(
        IProcessRunner processRunner,
        OperationLogService logs,
        IScrcpyToolLocator? scrcpyToolLocator = null,
        IScrcpyProvisioningService? provisioner = null)
    {
        this.processRunner = processRunner;
        this.logs = logs;
        this.scrcpyToolLocator = scrcpyToolLocator ?? new ScrcpyToolLocator(AppContext.BaseDirectory);
        this.provisioner = provisioner;
        synchronizationContext = SynchronizationContext.Current;
    }

    public bool AutoMirrorEnabled { get; set; }

    public bool IsMirroring => process is { HasExited: false };

    public bool IsToolAvailable => scrcpyToolLocator.IsAvailable;

    public string ToolStatusMessage => scrcpyToolLocator.StatusMessage;

    public void ConfigureToolPath(string toolPath) => scrcpyToolLocator.ConfigureToolPath(toolPath);

    public event EventHandler? StateChanged;

    public async Task ReconcileAsync(DeviceSessionViewModel session, CancellationToken cancellationToken)
    {
        trackedSession = session;
        if (AutoMirrorEnabled && !deliberateStop && session.ConnectionState == DeviceConnectionState.AdbConnected && !IsMirroring)
        {
            await StartCoreAsync(session, cancellationToken);
        }
    }

    public Task StartAsync(DeviceSessionViewModel session, CancellationToken cancellationToken)
    {
        trackedSession = session;
        deliberateStop = false;
        consecutiveRestartFailures = 0;
        return StartCoreAsync(session, cancellationToken);
    }

    public void ClearDeliberateStop()
    {
        deliberateStop = false;
        consecutiveRestartFailures = 0;
    }

    public Task StopAsync()
    {
        deliberateStop = true;
        consecutiveRestartFailures = 0;
        var activeProcess = process;
        process = null;
        if (activeProcess is not null)
        {
            activeProcess.Exited -= OnProcessExited;
            activeProcess.Stop();
            activeProcess.Dispose();
        }

        logs.Write(OperationLogLevel.Info, "已结束 ADB 投屏。");
        NotifyStateChanged();
        return Task.CompletedTask;
    }

    private async Task StartCoreAsync(DeviceSessionViewModel session, CancellationToken cancellationToken)
    {
        cancellationToken.ThrowIfCancellationRequested();

        if (session.ConnectionState != DeviceConnectionState.AdbConnected)
        {
            logs.Write(OperationLogLevel.Warning, "ADB 设备未就绪，无法启动投屏。");
            return;
        }

        if (!scrcpyToolLocator.IsAvailable && !await TryProvisionAsync(cancellationToken))
        {
            return;
        }

        if (IsMirroring)
        {
            return;
        }

        try
        {
            var executable = scrcpyToolLocator.ExecutablePath!;
            process = processRunner.Start(executable, ["--serial", session.Serial, "--stay-awake"]);
            process.Exited += OnProcessExited;
            logs.Write(OperationLogLevel.Success, $"ADB 投屏已启动: {session.Serial}。");
            NotifyStateChanged();
        }
        catch (Exception exception)
        {
            logs.Write(OperationLogLevel.Error, $"启动 ADB 投屏失败: {exception.Message}");
        }
    }

    private async Task<bool> TryProvisionAsync(CancellationToken cancellationToken)
    {
        if (provisioner is null)
        {
            logs.Write(OperationLogLevel.Warning, $"{scrcpyToolLocator.StatusMessage}；内置 scrcpy 资源未随程序发布，无法启动投屏。请重新执行发布脚本。");
            return false;
        }

        try
        {
            logs.Write(OperationLogLevel.Info, "未找到 scrcpy，正在从 GitHub 下载官方发行版…");
            var executable = await provisioner.EnsureInstalledAsync(cancellationToken);
            ConfigureToolPath(executable);
            return true;
        }
        catch (OperationCanceledException)
        {
            return false;
        }
        catch (Exception exception)
        {
            logs.Write(OperationLogLevel.Error, $"自动下载 scrcpy 失败: {exception.Message}");
            return false;
        }
    }

    private void OnProcessExited(object? sender, EventArgs eventArgs)
    {
        if (sender is not IRunningProcess exitedProcess)
        {
            return;
        }

        if (synchronizationContext is not null && SynchronizationContext.Current != synchronizationContext)
        {
            synchronizationContext.Post(_ => HandleProcessExited(exitedProcess), null);
            return;
        }

        HandleProcessExited(exitedProcess);
    }

    private void HandleProcessExited(IRunningProcess exitedProcess)
    {
        if (!ReferenceEquals(process, exitedProcess))
        {
            return;
        }

        exitedProcess.Exited -= OnProcessExited;
        exitedProcess.Dispose();
        process = null;
        logs.Write(OperationLogLevel.Info, "ADB 投屏窗口已关闭。");
        NotifyStateChanged();

        if (!AutoMirrorEnabled || deliberateStop || trackedSession?.ConnectionState != DeviceConnectionState.AdbConnected)
        {
            return;
        }

        consecutiveRestartFailures++;
        if (consecutiveRestartFailures > MaxConsecutiveRestartFailures)
        {
            logs.Write(OperationLogLevel.Warning, "ADB 投屏连续异常退出，已停止自动恢复。请手动重启投屏。");
            NotifyStateChanged();
            return;
        }

        _ = RestartAfterExitAsync();
    }

    private async Task RestartAfterExitAsync()
    {
        try
        {
            await Task.Delay(TimeSpan.FromSeconds(1));
            if (!deliberateStop && trackedSession is not null)
            {
                await ReconcileAsync(trackedSession, CancellationToken.None);
            }
        }
        catch (Exception exception)
        {
            logs.Write(OperationLogLevel.Error, $"自动恢复 ADB 投屏失败: {exception.Message}");
        }
    }

    private void NotifyStateChanged() => StateChanged?.Invoke(this, EventArgs.Empty);

}
