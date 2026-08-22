using VivoKsu.App.Models;

namespace VivoKsu.App.Services;

/// <summary>服务端拒绝用户操作(账号封禁/停用等)。页面据此提示并阻止操作开始。</summary>
public sealed class OperationDeniedException : InvalidOperationException
{
    public OperationDeniedException(string message)
        : base(message)
    {
    }
}

/// <summary>
/// 基于服务端的操作许可门禁。策略:服务端返回 allowed:false(封禁/停用)→ 拒绝;
/// token 无效(401)→ 拒绝;网络/服务端临时错误 → <b>默认放行</b>(服务端不可达不应阻塞刷写,
/// 账号封禁由心跳 5s 内强制退出兜底)。
/// </summary>
public sealed class ServerOperationGate : IOperationPermissionGate
{
    /// <summary>授权请求有界超时:黑洞/丢包时最长等这么久后默认放行,不长时间占用全局操作门禁。</summary>
    public static readonly TimeSpan AuthorizeTimeout = TimeSpan.FromSeconds(5);

    private readonly OtaApiClient client;

    public ServerOperationGate(OtaApiClient client)
    {
        this.client = client;
    }

    public async Task<OperationAuthorization> AuthorizeAsync(OperationKind kind, string title, CancellationToken cancellationToken)
    {
        using var timeoutCts = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        timeoutCts.CancelAfter(AuthorizeTimeout);
        try
        {
            return await client.AuthorizeOperationAsync(kind.ToString(), title, timeoutCts.Token).ConfigureAwait(false);
        }
        catch (OperationCanceledException) when (cancellationToken.IsCancellationRequested)
        {
            throw;
        }
        catch (OperationCanceledException)
        {
            // 授权请求超时:默认放行(服务端默认许可;网络黑洞不应阻塞刷写)。
            return OperationAuthorization.Allow();
        }
        catch (OtaApiException exception) when (exception.StatusCode == 401)
        {
            // token 失效/账号停用:拒绝。
            return OperationAuthorization.Deny("登录已失效,请联系管理员。");
        }
        catch (OtaApiException exception) when (exception.StatusCode == 426)
        {
            // 版本过低:拒绝操作(心跳会随后弹强制更新窗)。
            return OperationAuthorization.Deny("需要更新 VivoKsu 后才能继续使用。");
        }
        catch
        {
            // 网络抖动 / 服务端 5xx:默认放行(服务端默认许可;不可达不应阻塞设备操作)。
            return OperationAuthorization.Allow();
        }
    }
}
