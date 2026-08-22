namespace VivoKsu.App.Models;

/// <summary>服务端对一次用户操作是否放行(操作许可门禁)。</summary>
public sealed record OperationAuthorization(bool Allowed, string? Reason)
{
    public static OperationAuthorization Allow() => new(true, null);

    public static OperationAuthorization Deny(string reason) => new(false, reason);
}
