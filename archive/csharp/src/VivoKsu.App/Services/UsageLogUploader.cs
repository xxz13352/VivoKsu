using System.Threading;
using VivoKsu.App.Models;

namespace VivoKsu.App.Services;

/// <summary>
/// 客户端使用日志的批量上报器:线程安全收集 <see cref="UsageLogEntry"/>,
/// 每 <see cref="FlushInterval"/> 或缓冲超过 <see cref="FlushThreshold"/> 时上传到服务端,
/// 退出时由 <see cref="FlushAsync"/> 兜底。上传由信号量串行化(不会并发双传);
/// 失败批次放回缓冲,下次 flush 重试(best-effort,不阻塞任何操作)。
/// </summary>
public sealed class UsageLogUploader : IUsageReporter, IDisposable
{
    /// <summary>定时上传间隔。</summary>
    public static readonly TimeSpan FlushInterval = TimeSpan.FromSeconds(30);

    /// <summary>缓冲超过此数量立即上传。</summary>
    public const int FlushThreshold = 20;

    /// <summary>单批最多条数(对应服务端 /api/usage/logs 上限;保守取 D1 batch 语句上限)。</summary>
    public const int MaxBatchSize = 100;

    private readonly OtaApiClient client;
    private readonly object gate = new();
    private readonly List<UsageLogEntry> pending = [];
    private readonly SemaphoreSlim uploadGate = new(1, 1);
    private System.Threading.Timer? timer;
    private volatile bool disposed;

    public UsageLogUploader(OtaApiClient client)
    {
        this.client = client;
    }

    /// <summary>是否已启动定时上传(登录后由 AppComposition 调用)。</summary>
    public bool IsRunning => timer is not null;

    /// <summary>缓冲中的日志条数。</summary>
    public int PendingCount
    {
        get
        {
            lock (gate)
            {
                return pending.Count;
            }
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
        if (disposed)
        {
            return;
        }

        bool flushNow;
        lock (gate)
        {
            pending.Add(entry);
            flushNow = pending.Count >= FlushThreshold;
        }

        if (flushNow)
        {
            _ = FlushAsync();
        }
    }

    /// <summary>把缓冲中的日志上传到服务端。串行化:若有在途上传,等它完成后再取走剩余缓冲(退出时不丢尾批)。</summary>
    public async Task FlushAsync()
    {
        if (disposed)
        {
            return;
        }

        await uploadGate.WaitAsync().ConfigureAwait(false);
        try
        {
            UsageLogEntry[] batch;
            lock (gate)
            {
                if (pending.Count == 0)
                {
                    return;
                }

                batch = pending.ToArray();
                pending.Clear();
            }

            // 单批最多 MaxBatchSize(服务端上限),超出拆分。
            foreach (var chunk in batch.Chunk(MaxBatchSize))
            {
                try
                {
                    await client.UploadUsageLogsAsync(chunk, CancellationToken.None).ConfigureAwait(false);
                }
                catch
                {
                    // 失败:放回缓冲,下次 flush 重试(离线期间的操作记录不丢)。
                    lock (gate)
                    {
                        pending.InsertRange(0, chunk);
                    }
                }
            }
        }
        finally
        {
            uploadGate.Release();
        }
    }

    public void Dispose()
    {
        disposed = true;
        timer?.Dispose();
        timer = null;
        // 不 dispose uploadGate:退出时可能仍有在途/队列的 FlushAsync 在等它,
        // dispose 会抛 ObjectDisposedException;进程即将退出,让 GC 回收即可。
    }
}
