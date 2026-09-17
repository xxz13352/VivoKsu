namespace VivoKsu.App.Models;

/// <summary>完整性遥测的阶段(服务端闭集;越界一律 400)。线格式为 snake_case。</summary>
public enum IntegrityReportPhase
{
    Startup,
    Login,
    SessionRestore,
    Heartbeat,
    OperationAdmission,
    PinValidation,
}

/// <summary>完整性遥测的原因(服务端闭集;越界一律 400)。线格式为 snake_case。</summary>
public enum IntegrityReportReason
{
    ImageCrcInvalid,
    LeaseSignatureInvalid,
    LeaseBindingInvalid,
    LeaseExpired,
    SequenceRollback,
    PinMismatch,
    DebuggerDetected,
    VirtualMachineDetected,
    AuthenticodeInvalid,
    ReleaseManifestInvalid,
}

/// <summary>
/// <c>POST /api/integrity/report</c> 的请求体(与 Rust <c>IntegrityReportRequest</c> 同构)。
/// 服务端要求严格闭集字段(多一个/少一个字段都 400),因此这里用固定字段序列化,
/// 并在发送前做与 Rust <c>validate()</c> 一致的长度/字符集校验。
/// </summary>
public sealed record IntegrityReportRequest
{
    /// <summary>事件唯一键(服务端按此幂等去重):1-64 位 <c>[A-Za-z0-9._:-]</c>。</summary>
    public required string EventId { get; init; }

    public required IntegrityReportPhase Phase { get; init; }

    public required IntegrityReportReason Reason { get; init; }

    /// <summary>客户端版本号:1-32 位,首字符字母数字,其余允许 <c>. _ + -</c>。</summary>
    public required string ClientVersion { get; init; }

    /// <summary>构建标识:1-128 位 <c>[A-Za-z0-9._:-]</c>。</summary>
    public required string BuildId { get; init; }

    /// <summary>发生时间(Unix 秒)。服务端要求 1..2^53-1 的安全整数。</summary>
    public required long OccurredAtEpochSeconds { get; init; }
}
