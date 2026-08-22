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
