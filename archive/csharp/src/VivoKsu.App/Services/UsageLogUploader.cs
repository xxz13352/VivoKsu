using System.IO;
using System.Threading;
using VivoKsu.App.Models;

namespace VivoKsu.App.Services;

/// <summary>
/// 客户端使用日志的上报器(对齐 Rust <c>UsageLogReporter</c> + <c>LegacyUsageReporter</c>)。
/// <para>
/// 与旧实现(纯内存缓冲 + 无差别重试)的关键差异:
/// 1. <b>持久化</b>:待上传记录落在磁盘队列(见 <see cref="UsageLogSpool"/>),先落盘后上传,
///    上传成功才移除 —— 崩溃/强杀/离线退出不丢记录,下次启动自动续传。
/// 2. <b>账号隔离</b>:记录绑定归属(见 <see cref="UsageLogOwner"/>),切换账号后旧账号的队列
///    绝不会用新账号的 token 上传;同一账号重新登录可用新 token 续传旧队列。
/// 3. <b>错误分流</b>:4xx 视为永久失败(整批丢弃,避免毒丸批次无限重试),其余保留重试。
/// 4. <b>排空循环</b>:一次 flush 分批把队列传空,而不是只传当前缓冲一批。
/// 5. <b>退出预算</b>:<see cref="FlushAsync(TimeSpan, CancellationToken)"/> 带 deadline,
///    网络挂住时取消在途请求并保留队列(已落盘),不阻塞退出。
/// </para>
/// 上传串行化(不会并发双传);整体 best-effort,失败不抛出、不阻塞任何操作。
/// </summary>
public sealed class UsageLogUploader : IUsageReporter, IDisposable
{
    /// <summary>定时上传间隔。</summary>
    public static readonly TimeSpan FlushInterval = TimeSpan.FromSeconds(30);

    /// <summary>缓冲超过此数量立即上传。</summary>
    public const int FlushThreshold = 20;

    /// <summary>单批最多条数(服务端 /api/usage/logs 硬上限 100,超出返回 400)。</summary>
    public const int MaxBatchSize = 100;

    private readonly OtaApiClient client;
    private readonly UsageLogSpool spool;
    private readonly SemaphoreSlim uploadGate = new(1, 1);
    private readonly object gate = new();
    private System.Threading.Timer? timer;
    private UsageLogOwner owner = UsageLogOwner.Unbound;
    private bool sessionPublished;
    private volatile bool stopped;
    private volatile bool disposed;

    /// <param name="client">API 客户端(token 由外部注入;上传使用其当前 token,即当前登录账号的 token)。</param>
    /// <param name="spoolPath">
    /// 磁盘队列文件路径;传 null 表示仅内存(测试或不需要持久化)。
    /// 生产路径建议 <c>%LOCALAPPDATA%\VivoKsu\usage-logs.json</c>。
    /// </param>
    public UsageLogUploader(OtaApiClient client, string? spoolPath = null)
    {
        this.client = client;
        SpoolPath = spoolPath;
        spool = new UsageLogSpool(
            spoolPath ?? Path.Combine(Path.GetTempPath(), "VivoKsu", "usage-logs", $"{Guid.NewGuid():N}.json"));
    }

    /// <summary>磁盘队列路径;null 表示纯内存模式(不跨进程保留)。</summary>
    public string? SpoolPath { get; }

    /// <summary>是否已启动定时上传(登录后由 AppComposition 调用)。</summary>
    public bool IsRunning => timer is not null;

    /// <summary>队列中待上传的条数(全部归属)。</summary>
    public int PendingCount => spool.PendingCount;

    /// <summary>
    /// 绑定登录会话(对齐 Rust <c>publish_session</c>)。此后产生的记录归属该账号,上传只发送
    /// 同账号的记录;切换账号后旧队列留在磁盘,等待该账号下次登录续传。
    /// </summary>
    /// <param name="account">登录账号(内部立即做 SHA-256 不透明化,磁盘上不留明文)。</param>
    /// <param name="generation">登录代次(同一账号每次登录取不同值,可传会话 id)。</param>
    public void PublishSession(string account, string generation)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(account);
        ArgumentException.ThrowIfNullOrWhiteSpace(generation);

        lock (gate)
        {
            owner = UsageLogOwner.ForAccount(account, generation);
            sessionPublished = true;
        }
    }

    /// <summary>
    /// 结束当前登录会话:先给一次带预算的 flush,再解绑(对齐 Rust <c>flush_and_close_session</c>)。
    /// 此后 <see cref="Record"/> 不再接收新记录;上传失败的记录仍在磁盘,等该账号下次登录续传。
    /// </summary>
    public async Task CloseSessionAsync(TimeSpan? budget = null)
    {
        await FlushAsync(budget ?? TimeSpan.FromSeconds(5), CancellationToken.None).ConfigureAwait(false);

        lock (gate)
        {
            owner = UsageLogOwner.Unbound;
            sessionPublished = false;
        }
    }

    /// <summary>启动定时上传(幂等)。</summary>
    public void Start()
    {
        if (timer is not null)
        {
            return;
        }

        timer = new System.Threading.Timer(
            _ => _ = FlushAsync(),
            null,
            FlushInterval,
            FlushInterval);
    }

    public void Record(UsageLogEntry entry)
    {
        // 停止/已释放后一律丢弃:退出路径上不再产生新记录(对齐 Rust 的 stopped 短路)。
        if (disposed || stopped)
        {
            return;
        }

        bool flushNow;
        lock (gate)
        {
            // 未绑定登录会话时不记录:不知道归属哪个账号,上传只会用错 token 或让队列永久堆积。
            if (!sessionPublished)
            {
                return;
            }

            spool.Enqueue(owner, entry);
            flushNow = IsRunning && spool.PendingCount >= FlushThreshold;
        }

        if (flushNow)
        {
            _ = FlushAsync();
        }
    }

    /// <summary>把队列上传排空(无超时预算;供定时与阈值触发)。失败不抛出。</summary>
    public Task FlushAsync() => FlushAsync(Timeout.InfiniteTimeSpan, CancellationToken.None);

    /// <summary>
    /// 在给定预算内尽量把队列上传排空(对齐 Rust <c>flush_until</c>)。
    /// 预算耗尽时取消在途请求、保留队列(已落盘),不阻塞退出。失败不抛出。
    /// </summary>
    public async Task FlushAsync(TimeSpan budget, CancellationToken cancellationToken = default)
    {
        if (disposed)
        {
            return;
        }

        using var budgetCts = budget == Timeout.InfiniteTimeSpan ? null : new CancellationTokenSource(budget);
        using var linked = budgetCts is not null && cancellationToken.CanBeCanceled
            ? CancellationTokenSource.CreateLinkedTokenSource(budgetCts.Token, cancellationToken)
            : null;
        var token = linked?.Token ?? budgetCts?.Token ?? cancellationToken;

        await uploadGate.WaitAsync(token).ConfigureAwait(false);
        try
        {
            UsageLogOwner current;
            lock (gate)
            {
                if (!sessionPublished)
                {
                    return;
                }

                current = owner;
            }

            // 排空循环:分批发送,直到队列空、遇到临时失败或预算耗尽。
            while (!token.IsCancellationRequested)
            {
                var batch = spool.TakeBatch(current, MaxBatchSize);
                if (batch.Count == 0)
                {
                    return;
                }

                try
                {
                    await client.UploadUsageLogsAsync(batch, token).ConfigureAwait(false);
                    spool.RemoveUploaded(current, batch.Count);
                }
                catch (OtaApiException exception) when (exception.StatusCode is >= 400 and < 500)
                {
                    // 4xx = 服务端明确拒绝(结构非法/超上限/账号封禁):留着只会无限重试,整批丢弃。
                    spool.DiscardRejected(current, batch.Count);
                }
                catch
                {
                    // 网络错误 / 5xx / 超时:整批保留(含未尝试的尾部),下次 flush 重试。
                    return;
                }
            }
        }
        catch (OperationCanceledException)
        {
            // 预算耗尽:队列已落盘,下次启动续传。
        }
        finally
        {
            uploadGate.Release();
        }
    }

    /// <summary>
    /// 停止接收新记录并取消定时上传(退出路径;对齐 Rust 的 closeout)。
    /// 已入队记录仍在磁盘,不受影响。
    /// </summary>
    public void Stop()
    {
        stopped = true;
        timer?.Dispose();
        timer = null;
    }

    public void Dispose()
    {
        disposed = true;
        Stop();
        // 不 dispose uploadGate:退出时可能仍有在途/排队的 FlushAsync 在等它,
        // dispose 会抛 ObjectDisposedException;进程即将退出,交给 GC 回收。
        // 队列文件刻意保留:未传完的记录要留给下次启动续传。
    }
}
