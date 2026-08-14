using VivoKsu.App.Models;
using VivoKsu.App.ViewModels;

namespace VivoKsu.App.Services;

public sealed class OperationCoordinator : IOperationCoordinator, IDisposable
{
    /// <summary>另一个页面有任务在跑时,拒绝新操作并提示的文案。</summary>
    public const string OperationInProgressMessage = "已有任务正在进行中，请等待其完成或先取消。";

    private readonly DeviceSessionViewModel session;
    private readonly OperationLogService logs;
    private readonly SemaphoreSlim operationGate = new(1, 1);
    private readonly object stateGate = new();
    private readonly Action<string>? notifyBlocked;
    private OperationStateSnapshot state = OperationStateSnapshot.Idle;
    private CancellationTokenSource? currentCancellation;
    private long lastProgressReport;
    private bool disposed;

    public OperationCoordinator(
        DeviceSessionViewModel session,
        OperationLogService logs,
        Action<string>? notifyBlocked = null)
    {
        this.session = session;
        this.logs = logs;
        this.notifyBlocked = notifyBlocked;
    }

    public bool IsBusy
    {
        get
        {
            lock (stateGate)
            {
                return state.Kind is not OperationKind.Idle;
            }
        }
    }

    public OperationStateSnapshot State
    {
        get
        {
            lock (stateGate)
            {
                return state;
            }
        }
    }

    public event EventHandler? StateChanged;

    public async Task RunAsync(
        OperationKind kind,
        string title,
        Func<OperationContext, CancellationToken, Task> operation,
        CancellationToken cancellationToken = default)
    {
        ArgumentNullException.ThrowIfNull(operation);
        ObjectDisposedException.ThrowIf(disposed, this);

        // 全局只允许一个操作(刷写/下载/解包/重启等)。另一个页面有任务在跑时不再静默
        // 排队——那会让用户"点了没反应"——而是立即弹窗提示并抛错,由页面给出明确反馈。
        if (!operationGate.Wait(0))
        {
            notifyBlocked?.Invoke(OperationInProgressMessage);
            throw new OperationInProgressException(OperationInProgressMessage);
        }

        using var linkedCancellation = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        var operationId = Guid.NewGuid().ToString("N");
        SetCurrent(kind, operationId, title, title, null, linkedCancellation);
        session.BeginOperation(kind, title);
        logs.Write(OperationLogLevel.Info, title, operationId);

        try
        {
            var context = new OperationContext(operationId, Report);
            await operation(context, linkedCancellation.Token);
            session.CompleteOperation();
            logs.Write(OperationLogLevel.Success, $"{title}完成。", operationId);
        }
        catch (OperationCanceledException) when (linkedCancellation.IsCancellationRequested)
        {
            session.CancelOperation();
            logs.Write(OperationLogLevel.Warning, $"{title}已取消。", operationId);
            throw;
        }
        catch (Exception exception)
        {
            session.FailOperation($"{title}失败");
            logs.Write(OperationLogLevel.Error, exception.Message, operationId);
            throw;
        }
        finally
        {
            ClearCurrent(linkedCancellation);
            operationGate.Release();
        }
    }

    public void CancelCurrent()
    {
        CancellationTokenSource? cancellation;
        lock (stateGate)
        {
            cancellation = currentCancellation;
        }

        cancellation?.Cancel();
    }

    public void Dispose()
    {
        if (disposed)
        {
            return;
        }

        disposed = true;
        CancelCurrent();
    }

    private void SetCurrent(
        OperationKind kind,
        string operationId,
        string title,
        string stage,
        double? progress,
        CancellationTokenSource cancellation)
    {
        lock (stateGate)
        {
            currentCancellation = cancellation;
            state = new OperationStateSnapshot(
                kind,
                operationId,
                title,
                stage,
                progress,
                DateTimeOffset.Now,
                true);
        }

        StateChanged?.Invoke(this, EventArgs.Empty);
    }

    private void Report(string? stage, double? progress, OperationKind? kind)
    {
        OperationStateSnapshot previous;
        OperationStateSnapshot current;
        lock (stateGate)
        {
            previous = state;
            state = state with
            {
                Kind = kind ?? state.Kind,
                Stage = stage ?? state.Stage,
                Progress = progress ?? state.Progress
            };
            current = state;
        }

        var stageChanged = !string.Equals(current.Stage, previous.Stage, StringComparison.Ordinal)
            || current.Kind != previous.Kind
            || current.OperationId != previous.OperationId;
        var progressChanged = current.Progress != previous.Progress;

        if (stageChanged)
        {
            session.BeginOperation(current.Kind, current.Stage);
            if (current.OperationId is not null)
            {
                logs.Write(OperationLogLevel.Info, current.Stage, current.OperationId);
            }

            lastProgressReport = Environment.TickCount64;
            StateChanged?.Invoke(this, EventArgs.Empty);
            return;
        }

        if (!progressChanged)
        {
            return;
        }

        // Progress-only updates: surface StateChanged at most every 100ms so large
        // transfers do not flood the UI thread or the log.
        var now = Environment.TickCount64;
        if (now - lastProgressReport >= 100)
        {
            lastProgressReport = now;
            StateChanged?.Invoke(this, EventArgs.Empty);
        }
    }

    private void ClearCurrent(CancellationTokenSource cancellation)
    {
        lock (stateGate)
        {
            if (!ReferenceEquals(currentCancellation, cancellation))
            {
                return;
            }

            currentCancellation = null;
            state = OperationStateSnapshot.Idle;
        }

        StateChanged?.Invoke(this, EventArgs.Empty);
    }
}

/// <summary>全局已有一个操作在跑、新操作被拒绝时抛出,页面据此提示"工作正在进行"。</summary>
public sealed class OperationInProgressException : InvalidOperationException
{
    public OperationInProgressException(string message)
        : base(message)
    {
    }
}
