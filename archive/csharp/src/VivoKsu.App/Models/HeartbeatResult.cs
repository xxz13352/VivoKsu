namespace VivoKsu.App.Models;

/// <summary>
/// POST /api/heartbeat 的响应:服务端是否要求本进程强制退出。
/// Sequence 为响应租约载荷中的最新序号(服务端已推进到该值);goodbye 或载荷缺失时为 null。
/// </summary>
public sealed record HeartbeatResult(bool ForceExit, string? Reason, long? Sequence = null);
