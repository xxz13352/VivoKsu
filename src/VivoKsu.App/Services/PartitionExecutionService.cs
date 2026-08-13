using System.IO;
using VivoKsu.App.Models;
using VivoKsu.App.ViewModels;

namespace VivoKsu.App.Services;

public sealed class PartitionExecutionService
{
    private readonly DeviceSessionViewModel session;
    private readonly IOperationCoordinator coordinator;
    private readonly IReadOnlyDictionary<PartitionTransportKind, IPartitionTransport> transports;

    public PartitionExecutionService(
        DeviceSessionViewModel session,
        IOperationCoordinator coordinator,
        OperationLogService logs,
        IEnumerable<IPartitionTransport> transports)
    {
        this.session = session;
        this.coordinator = coordinator;
        this.transports = transports.ToDictionary(transport => transport.Kind);
    }

    public Task ExecuteAsync(
        PartitionExecutionPlan plan,
        Action<string, PartitionTaskState> setRowState,
        Action<PartitionTransferProgress> setProgress,
        CancellationToken cancellationToken)
    {
        ArgumentNullException.ThrowIfNull(plan);
        ArgumentNullException.ThrowIfNull(setRowState);
        ArgumentNullException.ThrowIfNull(setProgress);
        VerifySession(plan);

        if (!transports.TryGetValue(plan.Transport, out var transport))
        {
            throw new InvalidOperationException($"未配置 {plan.Transport} 分区通道。");
        }

        return coordinator.RunAsync(
            OperationKind.Flashing,
            GetOperationTitle(plan.Operation),
            (context, token) => ExecuteCoreAsync(plan, transport, context, setRowState, setProgress, token),
            cancellationToken);
    }

    private async Task ExecuteCoreAsync(
        PartitionExecutionPlan plan,
        IPartitionTransport transport,
        OperationContext context,
        Action<string, PartitionTaskState> setRowState,
        Action<PartitionTransferProgress> setProgress,
        CancellationToken cancellationToken)
    {
        var started = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        try
        {
            for (var index = 0; index < plan.Tasks.Count; index++)
            {
                cancellationToken.ThrowIfCancellationRequested();
                VerifySession(plan);

                var task = plan.Tasks[index];
                started.Add(task.PartitionName);
                setRowState(task.PartitionName, PartitionTaskState.Running);
                context.ReportStage($"正在{GetOperationLabel(plan.Operation)} {task.PartitionName}", OperationKind.Flashing);
                var progress = new InlineProgress<PartitionTransferProgress>(value =>
                {
                    var totalBytes = value.TotalBytes ?? task.SizeBytes;
                    setProgress(value with { PartitionName = task.PartitionName, TotalBytes = totalBytes });
                    var taskProgress = totalBytes is > 0 ? Math.Clamp(value.TransferredBytes / (double)totalBytes.Value, 0, 1) : 0;
                    context.ReportProgress((index + taskProgress) / plan.Tasks.Count);
                });

                try
                {
                    await ExecuteTaskAsync(plan, transport, task, progress, cancellationToken);
                    setRowState(task.PartitionName, PartitionTaskState.Succeeded);
                    context.ReportProgress((index + 1d) / plan.Tasks.Count);
                }
                catch (OperationCanceledException)
                {
                    setRowState(task.PartitionName, PartitionTaskState.Canceled);
                    throw;
                }
                catch
                {
                    setRowState(task.PartitionName, PartitionTaskState.Failed);
                    throw;
                }
            }
        }
        catch
        {
            // Any task that never started must not stay "Waiting" forever; mark it
            // as canceled so the row reflects that it was skipped.
            foreach (var task in plan.Tasks)
            {
                if (!started.Contains(task.PartitionName))
                {
                    setRowState(task.PartitionName, PartitionTaskState.Canceled);
                }
            }

            throw;
        }
    }

    private static async Task ExecuteTaskAsync(
        PartitionExecutionPlan plan,
        IPartitionTransport transport,
        PartitionTask task,
        IProgress<PartitionTransferProgress> progress,
        CancellationToken cancellationToken)
    {
        switch (plan.Operation)
        {
            case PartitionOperationKind.Write:
                await transport.WriteAsync(plan.Serial, task, progress, cancellationToken);
                return;
            case PartitionOperationKind.Erase:
                await transport.EraseAsync(plan.Serial, task, progress, cancellationToken);
                return;
            case PartitionOperationKind.Backup:
                await ExecuteBackupAsync(plan.Serial, transport, task, progress, cancellationToken);
                return;
            default:
                throw new InvalidOperationException($"未知分区操作: {plan.Operation}。");
        }
    }

    private static async Task ExecuteBackupAsync(
        string serial,
        IPartitionTransport transport,
        PartitionTask task,
        IProgress<PartitionTransferProgress> progress,
        CancellationToken cancellationToken)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(task.OutputPath);
        var directory = Path.GetDirectoryName(task.OutputPath);
        if (!string.IsNullOrWhiteSpace(directory))
        {
            Directory.CreateDirectory(directory);
        }

        var partialPath = task.OutputPath + ".partial";
        var partialTask = task with { OutputPath = partialPath };
        try
        {
            await transport.BackupAsync(serial, partialTask, progress, cancellationToken);
            File.Move(partialPath, task.OutputPath, overwrite: true);
        }
        catch
        {
            if (File.Exists(partialPath))
            {
                File.Delete(partialPath);
            }

            throw;
        }
    }

    private void VerifySession(PartitionExecutionPlan plan)
    {
        if (!string.Equals(session.Serial, plan.Serial, StringComparison.Ordinal))
        {
            throw new InvalidOperationException("连接设备已变化，请重新读取分区表后再执行。");
        }

        var expectedConnection = plan.Transport switch
        {
            PartitionTransportKind.AdbRoot => DeviceConnectionState.AdbConnected,
            PartitionTransportKind.Fastboot => DeviceConnectionState.FastbootConnected,
            _ => DeviceConnectionState.Disconnected
        };
        if (session.ConnectionState != expectedConnection)
        {
            throw new InvalidOperationException("当前设备连接状态与已确认的分区通道不一致。");
        }
    }

    private static string GetOperationTitle(PartitionOperationKind operation) => $"正在{GetOperationLabel(operation)}分区";

    private static string GetOperationLabel(PartitionOperationKind operation) => operation switch
    {
        PartitionOperationKind.Backup => "备份",
        PartitionOperationKind.Write => "写入",
        PartitionOperationKind.Erase => "擦除",
        _ => "执行"
    };

    private sealed class InlineProgress<T>(Action<T> report) : IProgress<T>
    {
        public void Report(T value) => report(value);
    }
}
