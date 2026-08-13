namespace VivoKsu.App.Models;

public sealed record OperationLogEntry(
    DateTimeOffset Timestamp,
    OperationLogLevel Level,
    string Message,
    string? OperationId = null)
{
    public string DisplayLevel => Level switch
    {
        OperationLogLevel.Info => "信息",
        OperationLogLevel.Success => "完成",
        OperationLogLevel.Warning => "注意",
        OperationLogLevel.Error => "失败",
        _ => "记录"
    };
}
