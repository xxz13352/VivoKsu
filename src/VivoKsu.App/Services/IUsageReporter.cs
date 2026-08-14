using VivoKsu.App.Models;

namespace VivoKsu.App.Services;

/// <summary>记录客户端使用日志,批量上传到服务端分类存储。</summary>
public interface IUsageReporter
{
    /// <summary>记录一条操作结果(线程安全,内部批量缓冲)。</summary>
    void Record(UsageLogEntry entry);

    /// <summary>把缓冲中的日志上传到服务端(失败不抛出;由实现决定重试/丢弃)。</summary>
    Task FlushAsync();
}
