namespace VivoKsu.App.Models;

/// <summary>POST /api/heartbeat 的响应:服务端是否要求本进程强制退出。</summary>
public sealed record HeartbeatResult(bool ForceExit, string? Reason);
