using System.Net.Http;
using System.Net.Http.Json;
using System.Text.Json;
using VivoKsu.App.Models;

namespace VivoKsu.App.Services;

/// <summary>
/// 桌面端登录:向 <c>api.nwflash.cc.cd/api/login</c> 提交账号密码,成功拿到 API token;
/// 并提供 <c>/api/me</c> 校验本地 token(记住登录)。
/// </summary>
public sealed class LoginService : IDisposable
{
    private readonly HttpClient http;
    private readonly string baseUrl;

    public LoginService(string baseUrl = OtaApiClient.DefaultBaseUrl)
    {
        this.baseUrl = baseUrl;
        http = new HttpClient();
        http.DefaultRequestHeaders.TryAddWithoutValidation("X-Nwflash-Version", AppInfo.Version);
    }

    /// <summary>账号密码登录。成功返回 token + 用户信息。</summary>
    public async Task<LoginResult> LoginAsync(string username, string password, CancellationToken cancellationToken)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(username);
        ArgumentException.ThrowIfNullOrWhiteSpace(password);

        // ConfigureAwait(false):App 启动门禁里会用 GetAwaiter().GetResult() 同步等待,
        // 若延续回到 UI 上下文会死锁(此时 Dispatcher 消息泵还没跑)。
        using var response = await http.PostAsJsonAsync(
            $"{baseUrl}/api/login", new { username, password }, cancellationToken).ConfigureAwait(false);
        var payload = await response.Content.ReadFromJsonAsync<JsonElement>(cancellationToken: cancellationToken).ConfigureAwait(false);

        // 版本过低 → 服务端 426,直接抛强制更新异常(由 App 全局捕获弹更新窗)。
        if (response.StatusCode == System.Net.HttpStatusCode.UpgradeRequired)
        {
            throw UpdateRequiredException.FromResponse(payload);
        }

        if (!response.IsSuccessStatusCode)
        {
            throw new LoginFailedException(
                payload.TryGetProperty("error", out var error) ? error.GetString() : $"登录失败({(int)response.StatusCode})");
        }

        var token = payload.TryGetProperty("token", out var t) ? t.GetString() : null;
        if (string.IsNullOrWhiteSpace(token))
        {
            throw new LoginFailedException("服务端未返回有效 token。");
        }

        var name = payload.TryGetProperty("name", out var n) ? n.GetString() : username;
        return new LoginResult(token, username, name ?? username);
    }

    /// <summary>校验本地 token 是否仍有效(记住登录)。返回显示名,无效返回 null。</summary>
    public async Task<string?> ValidateTokenAsync(string token, CancellationToken cancellationToken)
    {
        using var request = new HttpRequestMessage(HttpMethod.Get, $"{baseUrl}/api/me");
        request.Headers.TryAddWithoutValidation("Authorization", $"Bearer {token}");
        try
        {
            using var response = await http.SendAsync(request, cancellationToken).ConfigureAwait(false);
            if (response.StatusCode == System.Net.HttpStatusCode.UpgradeRequired)
            {
                var payload = await response.Content.ReadFromJsonAsync<JsonElement>(cancellationToken: cancellationToken).ConfigureAwait(false);
                throw UpdateRequiredException.FromResponse(payload);
            }
            var json = await response.Content.ReadFromJsonAsync<JsonElement>(cancellationToken: cancellationToken).ConfigureAwait(false);
            if (json.TryGetProperty("loggedIn", out var ok) && ok.GetBoolean())
            {
                return json.TryGetProperty("name", out var n) ? n.GetString() : null;
            }
        }
        catch (UpdateRequiredException) { throw; }
        catch
        {
            // 网络异常按未登录处理。
        }

        return null;
    }

    public void Dispose() => http.Dispose();
}

/// <summary>登录成功返回的结果。</summary>
public sealed record LoginResult(string Token, string Username, string Name);

/// <summary>账号密码错误 / 服务端拒绝登录。</summary>
public sealed class LoginFailedException : Exception
{
    public LoginFailedException(string message)
        : base(message)
    {
    }
}
