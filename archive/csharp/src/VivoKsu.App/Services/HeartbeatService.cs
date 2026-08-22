using VivoKsu.App.Models;

namespace VivoKsu.App.Services;

/// <summary>
/// 在线会话心跳:每 <see cref="HeartbeatInterval"/> 向服务端 POST 一次心跳并等待回应。
/// 用于证明「在线」、接收服务端指令(强制下线 / 强制更新 / 封禁)。
/// 网络抖动静默重试;force_exit / 426 / 401 / 403 会终止循环并交给回调处理。
///
/// 心跳与设备操作无关,不走 <see cref="OperationCoordinator"/>——设备操作串行不阻塞心跳,
/// 心跳也不得阻塞设备操作。循环严格串行(PeriodicTimer + 每请求独立超时),在途不发新请求。
/// </summary>
public sealed class HeartbeatService : IDisposable
{
    /// <summary>心跳间隔。服务端据此判定在线窗口(120s)与写节流(60s),此值需远小于二者。</summary>
    public static readonly TimeSpan HeartbeatInterval = TimeSpan.FromSeconds(5);

    /// <summary>单个心跳请求超时:请求挂起不得阻塞后续心跳(服务端按 120s 判在线,10s 超时足够)。</summary>
    public static readonly TimeSpan RequestTimeout = TimeSpan.FromSeconds(10);

    /// <summary>goodbye 请求超时:必须小于 App.OnExit 的 5s 关闭预算,否则网络慢时 goodbye 被进程退出截断。</summary>
    public static readonly TimeSpan GoodbyeTimeout = TimeSpan.FromSeconds(3);

    private readonly OtaApiClient client;
    private readonly Func<string, Task>? onForceExitAsync;
    private readonly Func<UpdateRequiredException, Task>? onUpdateRequiredAsync;
    private readonly TimeSpan heartbeatInterval;
    private CancellationTokenSource? cts;
    private Task? loop;
    private volatile string? sessionId;
    private volatile bool isHealthy;
    /// <summary>标记当前执行是否在循环的回调内(force_exit/426);供 StopAsync 判断能否等待循环。</summary>
    private readonly AsyncLocal<bool> inCallback = new();

    public HeartbeatService(
        OtaApiClient client,
        Func<string, Task>? onForceExitAsync = null,
        Func<UpdateRequiredException, Task>? onUpdateRequiredAsync = null,
        TimeSpan? heartbeatInterval = null)
    {
        this.client = client;
        this.onForceExitAsync = onForceExitAsync;
        this.onUpdateRequiredAsync = onUpdateRequiredAsync;
        this.heartbeatInterval = heartbeatInterval ?? HeartbeatInterval;
    }

    /// <summary>是否正在运行(Start 后、Stop 前)。</summary>
    public bool IsRunning => loop is not null;

    /// <summary>最近一次心跳是否成功。volatile bool,供 UI 定时器轮询读取(单读原子,无需事件跨线程)。</summary>
    public bool IsHealthy => isHealthy;

    /// <summary>启动心跳循环。重复调用为 no-op(会话只有一次生命周期)。</summary>
    public void Start(string newSessionId)
    {
        if (loop is not null)
        {
            return;
        }

        sessionId = newSessionId;
        cts = new CancellationTokenSource();
        loop = Task.Run(() => RunLoopAsync(cts.Token), CancellationToken.None);
    }

    /// <summary>
    /// 停止心跳:取消循环,并发发一次 goodbye(<c>active=false</c>)删掉会话行。幂等。
    /// goodbye 用全新短超时 token,绝不复用已取消的循环 token;失败不抛(退出路径不能被卡死)。
    /// 若从循环自身的回调内调用(force_exit / 426),跳过等待循环——此时循环在回调返回后自行结束,
    /// 若等待会「自己等自己」死锁;仅发送 goodbye 即可。
    /// </summary>
    public async Task StopAsync()
    {
        var current = cts;
        cts = null;
        current?.Cancel();

        var currentLoop = loop;
        loop = null;
        if (currentLoop is not null && !inCallback.Value)
        {
            try
            {
                await currentLoop.ConfigureAwait(false);
            }
            catch
            {
                // 取消即结束。
            }
        }

        await SendGoodbyeAsync();
    }

    /// <summary>发一次 goodbye(<c>active=false</c>)删掉会话行。全新短超时 token;失败不抛。</summary>
    public async Task SendGoodbyeAsync()
    {
        var id = sessionId;
        if (string.IsNullOrEmpty(id))
        {
            return;
        }

        using var goodbyeCts = new CancellationTokenSource(GoodbyeTimeout);
        try
        {
            await client.HeartbeatAsync(id, active: false, goodbyeCts.Token).ConfigureAwait(false);
        }
        catch
        {
            // 尽力而为;服务端 purge 兜底。
        }
    }

    private async Task RunLoopAsync(CancellationToken ct)
    {
        try
        {
            using var timer = new PeriodicTimer(heartbeatInterval);
            while (!ct.IsCancellationRequested)
            {
                try
                {
                    // 每请求独立超时;在途不发新请求(天然串行,无重叠)。
                    using var requestCts = CancellationTokenSource.CreateLinkedTokenSource(ct);
                    requestCts.CancelAfter(RequestTimeout);
                    var result = await client.HeartbeatAsync(sessionId!, active: true, requestCts.Token).ConfigureAwait(false);
                    isHealthy = true;

                    if (result.ForceExit)
                    {
                        if (onForceExitAsync is not null)
                        {
                            await RunCallbackAsync(() => onForceExitAsync(result.Reason ?? "已被服务端强制下线。")).ConfigureAwait(false);
                        }

                        return;
                    }
                }
                catch (UpdateRequiredException update)
                {
                    // 服务端 426:先停循环,再交回调弹更新窗(后台线程异常到不了 DispatcherUnhandledException)。
                    if (onUpdateRequiredAsync is not null)
                    {
                        await RunCallbackAsync(() => onUpdateRequiredAsync(update)).ConfigureAwait(false);
                    }

                    return;
                }
                catch (OtaApiException exception) when (exception.StatusCode is 401 or 403)
                {
                    // token 失效 / 账号停用 / 封禁:等同强制退出(不是静默继续)。
                    if (onForceExitAsync is not null)
                    {
                        await RunCallbackAsync(() => onForceExitAsync("账号已被停用或登录失效,请联系管理员。")).ConfigureAwait(false);
                    }

                    return;
                }
                catch (OperationCanceledException) when (ct.IsCancellationRequested)
                {
                    break;
                }
                catch
                {
                    // 网络抖动 / 服务端临时错误:静默,下个周期重试。
                    isHealthy = false;
                }

                try
                {
                    await timer.WaitForNextTickAsync(ct).ConfigureAwait(false);
                }
                catch (OperationCanceledException)
                {
                    break;
                }
            }
        }
        finally
        {
            // 循环自终止(force_exit / 426 / 401 / 403)后,IsRunning 应反映真实状态;Start 也不再被阻塞。
            loop = null;
        }
    }

    /// <summary>在循环内执行回调,并标记「回调中」(StopAsync 据此跳过等待循环,避免自己等自己死锁)。</summary>
    private async Task RunCallbackAsync(Func<Task> callback)
    {
        inCallback.Value = true;
        try
        {
            await InvokeSafelyAsync(callback).ConfigureAwait(false);
        }
        finally
        {
            inCallback.Value = false;
        }
    }

    private async Task InvokeSafelyAsync(Func<Task> callback)
    {
        try
        {
            await callback().ConfigureAwait(false);
        }
        catch
        {
            // 回调(弹窗/退出)失败不得让心跳任务崩溃;进程生命周期由回调自身负责。
        }
    }

    public void Dispose() => cts?.Cancel();
}
