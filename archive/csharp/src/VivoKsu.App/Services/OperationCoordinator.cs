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
    private readonly IOperationPermissionGate? permissionGate;
    private readonly IUsageReporter? usageReporter;
    private OperationStateSnapshot state = OperationStateSnapshot.Idle;
    private CancellationTokenSource? currentCancellation;
    private long lastProgressReport;
    private bool disposed;

    public OperationCoordinator(
        DeviceSessionViewModel session,
        OperationLogService logs,
        Action<string>? notifyBlocked = null,
        IOperationPermissionGate? permissionGate = null,
        IUsageReporter? usageReporter = null)
    {
        this.session = session;
        this.logs = logs;
        this.notifyBlocked = notifyBlocked;
        this.permissionGate = permissionGate;
        this.usageReporter = usageReporter;
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
        // 授权阶段就登记取消源:Stop 在授权等待期(最长 5s)也能取消,而不是「点了没反应」,
        // 之后操作仍照常开始。授权被取消/拒绝时由 finally 的 ClearCurrent 清理。
        lock (stateGate)
        {
            currentCancellation = linkedCancellation;
        }

        string? operationId = null;
        var startedAt = 0L;
        var startedMs = 0L;
        try
        {
            // 操作前服务端许可:默认放行;封禁/停用拒绝。授权在 try 内,拒绝/取消时操作门禁也能释放。
            if (permissionGate is not null)
            {
                var permission = await permissionGate.AuthorizeAsync(kind, title, linkedCancellation.Token);
                if (!permission.Allowed)
                {
                    var denied = $"服务端未许可此操作: {permission.Reason ?? "账号状态异常"}";
                    logs.Write(OperationLogLevel.Warning, denied);
                    // 多数页面吞掉 OperationDeniedException,只靠日志用户看不到;走 notifyBlocked(生产=MessageBox)
                    // 让「操作被拒」对用户可见——与 OperationInProgress 的提示同一条路径。
                    notifyBlocked?.Invoke(denied);
                    throw new OperationDeniedException(denied);
                }
            }

            operationId = Guid.NewGuid().ToString("N");
            startedAt = DateTimeOffset.UtcNow.ToUnixTimeSeconds();
            startedMs = Environment.TickCount64;
            SetCurrent(kind, operationId, title, title, null, linkedCancellation);
            session.BeginOperation(kind, title);
            logs.Write(OperationLogLevel.Info, title, operationId);

            var context = new OperationContext(operationId, Report);
            await operation(context, linkedCancellation.Token);
            session.CompleteOperation();
            logs.Write(OperationLogLevel.Success, $"{title}完成。", operationId);
            RecordUsage(kind, title, "success", startedAt, startedMs, operationId);
        }
        catch (OperationCanceledException) when (linkedCancellation.IsCancellationRequested)
        {
            // 授权阶段被取消时 operationId 为 null(操作未开始),不做状态/日志/使用记录。
            if (operationId is not null)
            {
                session.CancelOperation();
                logs.Write(OperationLogLevel.Warning, $"{title}已取消。", operationId);
                RecordUsage(kind, title, "canceled", startedAt, startedMs, operationId);
            }

            throw;
        }
        catch (OperationDeniedException)
        {
            // 已在授权处写日志并提示;操作门禁由 finally 释放。
            throw;
        }
        catch (Exception exception)
        {
            session.FailOperation($"{title}失败");
            logs.Write(OperationLogLevel.Error, exception.Message, operationId);
            RecordUsage(kind, title, "failed", startedAt, startedMs, operationId);
            throw;
        }
        finally
        {
            ClearCurrent(linkedCancellation);
            operationGate.Release();
        }
    }

    private void RecordUsage(OperationKind kind, string title, string status, long startedAt, long startedMs, string? operationId)
    {
        // operationId 为 null 说明失败发生在授权阶段(操作从未真正开始),不算一次真实操作,
        // 不上报使用日志(也避免生成一条无关联的幽灵记录)。
        if (usageReporter is null || operationId is null)
        {
            return;
        }

        var endedAt = DateTimeOffset.UtcNow.ToUnixTimeSeconds();
        var durationMs = Environment.TickCount64 - startedMs;
        // event_id 每次操作生成一次;上传重试复用同一条记录(同一 id),服务端幂等去重,不会重复落库。
        usageReporter.Record(new UsageLogEntry(
            kind.ToString(), title, status, Guid.NewGuid().ToString("N"), startedAt, endedAt, durationMs));
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
