namespace VivoKsu.Server.Services;

/// <summary>上游 VOTA API 返回失败时的业务异常,携带错误码供端点映射 HTTP 状态。</summary>
public sealed class RomResolveException : Exception
{
    public RomResolveException(string message, string? errorCode = null)
        : base(string.IsNullOrWhiteSpace(errorCode) ? message : $"{message}({errorCode})")
    {
        ErrorCode = errorCode;
    }

    /// <summary>VOTA 错误码,如 NOT_FOUND / AUTH_FAIL / INSUFFICIENT_CREDITS / RATE_LIMITED。</summary>
    public string? ErrorCode { get; }
}
