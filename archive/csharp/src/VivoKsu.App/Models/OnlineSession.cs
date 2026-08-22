namespace VivoKsu.App.Models;

/// <summary>
/// GET /api/online 返回的一个在线会话(客户端视角)。服务端刻意不返回登录 username/IP/user_id,
/// 只给显示名、版本与时长,避免把全部账号与活跃时段暴露给任意持 token 用户。
/// </summary>
public sealed record OnlineSession(
    string Name,
    string ClientVersion,
    long ConnectedAtEpochSeconds,
    long LastSeenAtEpochSeconds,
    long DurationSeconds,
    bool IsSelf);
