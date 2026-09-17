using System.Globalization;
using System.IO;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using VivoKsu.App.Models;

namespace VivoKsu.App.Services;

/// <summary>
/// 使用日志队列的归属标识(对齐 Rust <c>ReporterOwner</c>):一条待上传记录只属于某个账号。
/// 切换账号后,旧账号的队列不会被新账号的 token 上传(避免跨账号污染);
/// 同一账号的新 generation(重新登录)可以用新 token 续传旧队列。
/// </summary>
/// <param name="Account">账号的不透明标识(SHA-256 摘要,绝不含明文账号)。</param>
/// <param name="Generation">登录代次(同一账号不同登录会话取不同值)。</param>
public sealed record UsageLogOwner(string Account, long Generation)
{
    /// <summary>未绑定登录会话时的默认归属(仅内存模式/测试用)。</summary>
    public static readonly UsageLogOwner Unbound = new(OpaqueAccount(string.Empty), 0);

    public bool SameAccount(UsageLogOwner other) => Account == other.Account;

    /// <summary>
    /// 按账号 + 登录代次派生归属标识。算法与 Rust 客户端完全一致
    /// (<c>sha256("nwflash-v1-usage-owner\0" + account)</c> /
    ///  <c>sha256("nwflash-v1-usage-generation\0" + generation)</c> 前 8 字节大端),
    /// 因此两侧落盘的队列文件语义相同,且磁盘上永不出现明文账号或 token。
    /// </summary>
    public static UsageLogOwner ForAccount(string account, string generation) =>
        new(OpaqueAccount(account), BridgeGeneration(generation));

    private static string OpaqueAccount(string account)
    {
        var bytes = Encoding.UTF8.GetBytes("nwflash-v1-usage-owner\0" + account);
        return Convert.ToHexString(SHA256.HashData(bytes)).ToLowerInvariant();
    }

    private static long BridgeGeneration(string generation)
    {
        var bytes = Encoding.UTF8.GetBytes("nwflash-v1-usage-generation\0" + generation);
        var digest = SHA256.HashData(bytes);
        var value = System.Buffers.Binary.BinaryPrimitives.ReadUInt64BigEndian(digest);
        // Rust 侧 .max(1):0 保留给「未绑定」。
        return (long)Math.Max((ulong)value, 1UL);
    }
}

/// <summary>Spool 文件里的一条记录:归属 + 载荷。</summary>
public sealed record QueuedUsageEntry(UsageLogOwner Owner, UsageLogEntry Entry);

/// <summary>
/// 使用日志的持久化队列(对齐 Rust <c>LegacyUsageReporter</c> 的磁盘 spool)。
/// 待上传记录先落盘再上传,上传成功才移除 —— 进程崩溃、强杀、离线退出都不会丢历史操作记录,
/// 下次启动自动续传。写盘是原子的(tmp → flush → 原子替换),不会留下半截 JSON。
/// </summary>
public sealed class UsageLogSpool
{
    private static readonly JsonSerializerOptions SerializerOptions = new()
    {
        PropertyNameCaseInsensitive = true,
        WriteIndented = false,
    };

    private readonly string path;
    private readonly object gate = new();
    private readonly List<QueuedUsageEntry> pending;

    /// <param name="path">队列文件路径;父目录不存在时自动创建。</param>
    public UsageLogSpool(string path)
    {
        this.path = path;
        pending = Load(path);
    }

    /// <summary>队列中待上传的条数(全部归属)。</summary>
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

    /// <summary>入队并立即落盘(先持久化,再谈上传 —— 崩溃最多重复一条,不会丢)。</summary>
    public void Enqueue(UsageLogOwner owner, UsageLogEntry entry)
    {
        lock (gate)
        {
            pending.Add(new QueuedUsageEntry(owner, entry));
            Persist();
        }
    }

    /// <summary>取出属于 <paramref name="owner"/> 账号的前 <paramref name="max"/> 条(不移除)。</summary>
    public IReadOnlyList<UsageLogEntry> TakeBatch(UsageLogOwner owner, int max)
    {
        lock (gate)
        {
            return pending
                .Where(item => item.Owner.SameAccount(owner))
                .Take(max)
                .Select(item => item.Entry)
                .ToList();
        }
    }

    /// <summary>上传成功后移除队首属于该账号的 <paramref name="count"/> 条并重新落盘。</summary>
    public void RemoveUploaded(UsageLogOwner owner, int count)
    {
        lock (gate)
        {
            RemoveOwned(owner, count);
            Persist();
        }
    }

    /// <summary>
    /// 永久失败(4xx)时丢弃该账号的 <paramref name="count"/> 条。
    /// 对齐 Rust 的 <c>UploadError::Permanent</c>:服务端已明确拒绝,留着只会形成
    /// 「永远重试、永远失败」的毒丸批次。
    /// </summary>
    public void DiscardRejected(UsageLogOwner owner, int count)
    {
        lock (gate)
        {
            RemoveOwned(owner, count);
            Persist();
        }
    }

    private void RemoveOwned(UsageLogOwner owner, int count)
    {
        var removed = 0;
        for (var index = 0; index < pending.Count && removed < count; index++)
        {
            if (!pending[index].Owner.SameAccount(owner))
            {
                continue;
            }

            pending.RemoveAt(index);
            index--;
            removed++;
        }
    }

    private void Persist()
    {
        try
        {
            var directory = Path.GetDirectoryName(path);
            if (!string.IsNullOrEmpty(directory))
            {
                Directory.CreateDirectory(directory);
            }

            var temporary = path + ".tmp";
            using (var stream = new FileStream(temporary, FileMode.Create, FileAccess.Write, FileShare.None))
            {
                JsonSerializer.Serialize(stream, pending, SerializerOptions);
                // flush(true)=把 OS 页缓存也刷到盘:进程被强杀时 tmp 文件不是半截 JSON。
                stream.Flush(flushToDisk: true);
            }

            File.Move(temporary, path, overwrite: true);
        }
        catch
        {
            // 落盘失败(只读/磁盘满)不影响内存队列:记录仍可在本次进程内上传。
        }
    }

    private static List<QueuedUsageEntry> Load(string path)
    {
        try
        {
            if (!File.Exists(path))
            {
                return [];
            }

            using var stream = File.OpenRead(path);
            return JsonSerializer.Deserialize<List<QueuedUsageEntry>>(stream, SerializerOptions) ?? [];
        }
        catch
        {
            // 队列文件损坏(半截/旧版本格式):宁可丢掉队列也不让上报器崩掉启动。
            return [];
        }
    }

    /// <summary>仅供诊断:队列文件路径。</summary>
    public override string ToString() => string.Format(CultureInfo.InvariantCulture, "UsageLogSpool({0}, {1} pending)", path, PendingCount);
}
