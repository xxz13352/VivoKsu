namespace VivoKsu.App.Models;

/// <summary>
/// <c>POST /api/diagnostics/crash</c> 的请求体(与 Rust <c>CrashReportRequest</c> 同构)。
/// 上次进程崩溃的补传报告:客户端下次启动时上报。匿名(未登录)也可报,
/// 服务端按 event_id 幂等 + 按 IP 窗口限流(600s 内 5 条)。
/// </summary>
public sealed record CrashReportRequest
{
    /// <summary>panic_message 最大字节数(服务端上限)。</summary>
    public const int MaxPanicMessageBytes = 16 * 1024;

    /// <summary>backtrace 最大字节数(服务端上限)。</summary>
    public const int MaxBacktraceBytes = 32 * 1024;

    /// <summary>事件唯一键(服务端幂等):1-64 位 <c>[A-Za-z0-9._:-]</c>。</summary>
    public required string EventId { get; init; }

    /// <summary>客户端版本号:1-32 位,首字符字母数字,其余允许 <c>. _ + -</c>。</summary>
    public required string ClientVersion { get; init; }

    /// <summary>构建标识:1-128 位 <c>[A-Za-z0-9._:-]</c>。</summary>
    public required string BuildId { get; init; }

    /// <summary>崩溃发生时(或本次启动分配的)会话 id:1-64 位 <c>[A-Za-z0-9._:-]</c>。</summary>
    public required string SessionId { get; init; }

    /// <summary>panic 文本。非空且不超过 <see cref="MaxPanicMessageBytes"/>。调用方必须先做价值过滤(凭据/私钥等)。</summary>
    public required string PanicMessage { get; init; }

    /// <summary>调用栈。可为空,不超过 <see cref="MaxBacktraceBytes"/>。</summary>
    public required string Backtrace { get; init; }

    /// <summary>发生时间(Unix 秒)。服务端要求 1..2^53-1 的安全整数。</summary>
    public required long OccurredAtEpochSeconds { get; init; }
}

/// <summary><c>POST /api/usage/logs</c> 的响应(与 Rust <c>UsageLogUploadResponse</c> 同构)。</summary>
public sealed record UsageLogUploadResult(bool Ok, long? Received);
