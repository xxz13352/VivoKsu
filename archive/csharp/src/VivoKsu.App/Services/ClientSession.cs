namespace VivoKsu.App.Services;

/// <summary>
/// 新版登录契约(2026-08-23 签名租约)要求的进程/会话绑定标识:
/// - build_id:同一构建恒定(随应用版本变化),服务端租约绑定校验用;
/// - process_nonce:每次进程启动随机,防止同一构建的其它进程复用会话;
/// - session_id:每次登录生成,登录与后续心跳/下线必须携带同一值。
/// 服务端校验格式:build_id/process_nonce 最长 128,session_id 最长 64,
/// 仅允许 <c>A-Za-z0-9._:-</c>;GUID "N" 格式(32 位十六进制)满足全部约束。
/// </summary>
public static class ClientSession
{
    /// <summary>构建标识:随应用版本变化(如 "vivoksu-1.1.0"),进程生命周期内恒定。</summary>
    public static string BuildId { get; } = $"vivoksu-{AppInfo.Version}";

    /// <summary>进程随机 nonce(每次启动不同)。</summary>
    public static string ProcessNonce { get; } = Guid.NewGuid().ToString("N");

    /// <summary>生成新的会话 id(32 位十六进制 GUID)。</summary>
    public static string NewSessionId() => Guid.NewGuid().ToString("N");
}
