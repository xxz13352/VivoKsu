using System.Collections.ObjectModel;
using System.IO;
using System.Text;
using VivoKsu.App.Models;

namespace VivoKsu.App.Services;

/// <summary>
/// 操作日志:内存最近 500 条(UI 面板)+ 可选磁盘持久化(排查用)。
/// 磁盘日志格式 <c>[yyyy-MM-dd HH:mm:ss] [Level] 消息</c>,逐条追加;超容量后滚动为
/// <c>.1</c> 保留最近一段。UI「清空」只清内存面板,不清磁盘记录(磁盘是原始记录)。
/// </summary>
public sealed class OperationLogService
{
    private const int MaximumEntries = 500;
    private const long MaxLogFileBytes = 2L * 1024 * 1024;
    private readonly object writeLock = new();
    private readonly string? logFilePath;

    public ObservableCollection<OperationLogEntry> Entries { get; } = [];

    /// <param name="logFilePath">磁盘日志路径;null = 仅内存不落盘(测试/不需要持久化的组合)。</param>
    public OperationLogService(string? logFilePath = null)
    {
        this.logFilePath = logFilePath;
    }

    public void Write(OperationLogLevel level, string message, string? operationId = null)
    {
        var entry = new OperationLogEntry(DateTimeOffset.Now, level, message, operationId);
        Entries.Add(entry);

        while (Entries.Count > MaximumEntries)
        {
            Entries.RemoveAt(0);
        }

        Persist(entry);
    }

    /// <summary>清空内存面板;磁盘日志保留(持久化原始记录,便于事后排查)。</summary>
    public void Clear()
    {
        Entries.Clear();
    }

    /// <summary>
    /// 取某次操作的过程日志快照,供使用日志上报时作为 <c>details</c> 附带(对齐 Rust 的
    /// <c>UsageLogEntry::details</c>)。服务端只保留前 500 条、逐条正文截断到 16384 字符,
    /// 这里按同样上限裁剪,避免发出注定被丢弃的超大请求体。
    /// </summary>
    /// <param name="operationId">操作 id;null 或空返回空列表。</param>
    public IReadOnlyList<UsageLogDetail> SnapshotFor(string? operationId)
    {
        if (string.IsNullOrEmpty(operationId))
        {
            return [];
        }

        List<OperationLogEntry> matches;
        lock (writeLock)
        {
            matches = Entries
                .Where(entry => string.Equals(entry.OperationId, operationId, StringComparison.Ordinal))
                .ToList();
        }

        return matches
            .Take(MaximumUploadedDetails)
            .Select(entry => new UsageLogDetail(
                entry.Timestamp.ToUnixTimeSeconds(),
                entry.Level.ToString(),
                // 与服务端 slice(0, 16_384) 对齐:超长正文直接截断,不浪费带宽。
                entry.Message.Length <= MaxDetailMessageLength
                    ? entry.Message
                    : entry.Message[..MaxDetailMessageLength]))
            .ToList();
    }

    /// <summary>单条记录最多附带的明细条数(服务端硬上限)。</summary>
    private const int MaximumUploadedDetails = 500;

    /// <summary>单条明细正文的最大字符数(服务端硬上限)。</summary>
    private const int MaxDetailMessageLength = 16_384;

    private void Persist(OperationLogEntry entry)
    {
        if (logFilePath is null)
        {
            return;
        }

        try
        {
            lock (writeLock)
            {
                Directory.CreateDirectory(Path.GetDirectoryName(logFilePath)!);
                EnsureCapacity();
                File.AppendAllText(
                    logFilePath,
                    $"[{entry.Timestamp:yyyy-MM-dd HH:mm:ss}] [{entry.Level}] {entry.Message}{Environment.NewLine}",
                    Encoding.UTF8);
            }
        }
        catch
        {
            // 日志写失败(只读场景/磁盘满)不影响操作本身。
        }
    }

    /// <summary>超容量时把当前文件滚动为 .1(覆盖旧滚动),重新从空文件记起,磁盘占用有界。</summary>
    private void EnsureCapacity()
    {
        if (!File.Exists(logFilePath) || new FileInfo(logFilePath).Length < MaxLogFileBytes)
        {
            return;
        }

        File.Move(logFilePath, logFilePath + ".1", overwrite: true);
    }
}
