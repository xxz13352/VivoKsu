using System.Net.Http;
using System.Net.Http.Json;
using System.Text.Json;
using VivoKsu.App.Models;

namespace VivoKsu.App.Services;

/// <summary>
/// 查询 Cloudflare Worker(api.nwflash.cc.cd),按 PD + 版本号获取 OTA 下载链接。
/// 服务端(而非桌面端)持有 VOTA 凭据。
/// </summary>
public sealed class OtaApiClient
{
    private readonly HttpClient http;

    /// <summary>
    /// 默认构造:localhost 的自签 dev-cert 会被跳过证书校验;其它地址(如 Cloudflare 上的
    /// api.nwflash.cc.cd)要求合法证书(错误为空时同样放行)。
    /// </summary>
    public OtaApiClient(string baseUrl = DefaultBaseUrl)
        : this(CreateLocalHostClient(), baseUrl)
    {
    }

    public OtaApiClient(HttpClient http, string baseUrl = DefaultBaseUrl)
    {
        this.http = http;
        BaseUrl = baseUrl;
        // 每个请求带客户端版本号,服务端据此做强制更新校验。
        http.DefaultRequestHeaders.TryAddWithoutValidation("X-VivoKsu-Version", AppInfo.Version);
    }

    /// <summary>默认服务端地址:Cloudflare Worker 上的 VivoKsu ROM 代理(域名 nwflash.cc.cd)。</summary>
    public const string DefaultBaseUrl = "https://api.nwflash.cc.cd";

    /// <summary>服务端基地址,页面允许用户修改。</summary>
    public string BaseUrl { get; set; }

    /// <summary>登录后设置的 API token;设置后查询请求带 <c>Authorization: Bearer</c>。</summary>
    public string? Token
    {
        get => token;
        set
        {
            token = value;
            http.DefaultRequestHeaders.Remove("Authorization");
            if (!string.IsNullOrWhiteSpace(value))
            {
                http.DefaultRequestHeaders.TryAddWithoutValidation("Authorization", $"Bearer {value}");
            }
        }
    }

    private string? token;

    private static HttpClient CreateLocalHostClient()
    {
        var handler = new HttpClientHandler
        {
            ServerCertificateCustomValidationCallback = (request, _, _, errors) =>
                request?.RequestUri?.Host is "localhost" or "127.0.0.1"
                || errors == System.Net.Security.SslPolicyErrors.None
        };
        return new HttpClient(handler);
    }

    public async Task<RomInfo> ResolveAsync(string pd, string version, CancellationToken cancellationToken)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(pd);
        ArgumentException.ThrowIfNullOrWhiteSpace(version);

        var builder = new UriBuilder(BaseUrl)
        {
            Path = "/api/rom",
            Query = $"pd={Uri.EscapeDataString(pd)}&version={Uri.EscapeDataString(version)}"
        };

        using var response = await http.GetAsync(builder.Uri, HttpCompletionOption.ResponseHeadersRead, cancellationToken);
        if (response.StatusCode == System.Net.HttpStatusCode.UpgradeRequired)
        {
            var payload = await response.Content.ReadFromJsonAsync<JsonElement>(cancellationToken: cancellationToken).ConfigureAwait(false);
            throw UpdateRequiredException.FromResponse(payload);
        }
        if (!response.IsSuccessStatusCode)
        {
            throw await OtaApiException.FromResponseAsync(response);
        }

        var rom = await response.Content.ReadFromJsonAsync<RomInfo>(cancellationToken: cancellationToken);
        if (rom is null || string.IsNullOrWhiteSpace(rom.Url))
        {
            throw new OtaApiException("服务端返回了无效的 ROM 记录。", (int)response.StatusCode);
        }

        return rom;
    }

    /// <summary>
    /// 在线会话心跳:保持本实例「在线」并可接收服务端指令(强制下线 / 封禁 / 强制更新)。
    /// <paramref name="active"/>=false 为 goodbye,服务端删除会话行。
    /// </summary>
    public async Task<HeartbeatResult> HeartbeatAsync(string sessionId, bool active, CancellationToken cancellationToken)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(sessionId);

        using var response = await http.PostAsJsonAsync(
            $"{BaseUrl}/api/heartbeat",
            new { sessionId, clientVersion = AppInfo.Version, active },
            cancellationToken).ConfigureAwait(false);

        // 先判状态码再解析 body:空/非 JSON 响应体(如 WAF HTML 403)不得被当作网络抖动静默吞掉。
        if (response.StatusCode == System.Net.HttpStatusCode.UpgradeRequired)
        {
            var payload = await response.Content.ReadFromJsonAsync<JsonElement>(cancellationToken: cancellationToken).ConfigureAwait(false);
            throw UpdateRequiredException.FromResponse(payload);
        }
        if (!response.IsSuccessStatusCode)
        {
            throw await OtaApiException.FromResponseAsync(response);
        }

        var body = await response.Content.ReadFromJsonAsync<JsonElement>(cancellationToken: cancellationToken).ConfigureAwait(false);
        var forceExit = body.TryGetProperty("force_exit", out var fe)
            && fe.ValueKind == JsonValueKind.True;
        var reason = body.TryGetProperty("reason", out var r)
            ? r.GetString()
            : null;
        return new HeartbeatResult(forceExit, reason);
    }

    /// <summary>查询在线用户列表(显示名/版本/时长,不含 username/IP)。</summary>
    public async Task<IReadOnlyList<OnlineSession>> GetOnlineAsync(CancellationToken cancellationToken)
    {
        using var response = await http.GetAsync($"{BaseUrl}/api/online", cancellationToken).ConfigureAwait(false);
        if (response.StatusCode == System.Net.HttpStatusCode.UpgradeRequired)
        {
            var payload = await response.Content.ReadFromJsonAsync<JsonElement>(cancellationToken: cancellationToken).ConfigureAwait(false);
            throw UpdateRequiredException.FromResponse(payload);
        }
        if (!response.IsSuccessStatusCode)
        {
            throw await OtaApiException.FromResponseAsync(response);
        }

        var body = await response.Content.ReadFromJsonAsync<JsonElement>(cancellationToken: cancellationToken).ConfigureAwait(false);
        if (!body.TryGetProperty("sessions", out var sessions) || sessions.ValueKind != JsonValueKind.Array)
        {
            return [];
        }

        var list = new List<OnlineSession>();
        foreach (var item in sessions.EnumerateArray())
        {
            list.Add(new OnlineSession(
                GetString(item, "name"),
                GetString(item, "client_version"),
                GetInt64(item, "connected_at"),
                GetInt64(item, "last_seen_at"),
                GetInt64(item, "duration_seconds"),
                item.TryGetProperty("is_self", out var self) && self.ValueKind == JsonValueKind.True));
        }

        return list;
    }

    private static string GetString(JsonElement element, string name) =>
        element.TryGetProperty(name, out var value) && value.ValueKind == JsonValueKind.String
            ? value.GetString() ?? string.Empty
            : string.Empty;

    private static long GetInt64(JsonElement element, string name) =>
        element.TryGetProperty(name, out var value)
            && (value.ValueKind == JsonValueKind.Number || value.ValueKind == JsonValueKind.String)
            && value.TryGetInt64(out var number)
            ? number
            : 0;

    /// <summary>
    /// 操作许可门禁:客户端每个用户操作运行前询问。服务端默认放行;封禁/停用返回
    /// <c>{ allowed:false, reason }</c>;token 无效/停用返回 401(调用方按拒绝处理)。
    /// </summary>
    public async Task<OperationAuthorization> AuthorizeOperationAsync(string operation, string title, CancellationToken cancellationToken)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(operation);

        using var response = await http.PostAsJsonAsync(
            $"{BaseUrl}/api/operation/authorize",
            new { operation, title },
            cancellationToken).ConfigureAwait(false);
        if (!response.IsSuccessStatusCode)
        {
            throw await OtaApiException.FromResponseAsync(response);
        }

        var body = await response.Content.ReadFromJsonAsync<JsonElement>(cancellationToken: cancellationToken).ConfigureAwait(false);
        var allowed = body.TryGetProperty("allowed", out var value)
            && value.ValueKind == JsonValueKind.True;
        var reason = body.TryGetProperty("reason", out var r)
            ? r.GetString()
            : null;
        return allowed ? OperationAuthorization.Allow() : OperationAuthorization.Deny(reason ?? "服务端未许可此操作。");
    }

    /// <summary>批量上传使用日志(服务端按操作分类存储,绑定当前 token 用户)。失败抛异常,调用方按 best-effort 处理。</summary>
    public async Task UploadUsageLogsAsync(IReadOnlyList<UsageLogEntry> logs, CancellationToken cancellationToken)
    {
        if (logs.Count == 0)
        {
            return;
        }

        using var response = await http.PostAsJsonAsync(
            $"{BaseUrl}/api/usage/logs",
            new { logs },
            cancellationToken).ConfigureAwait(false);
        if (!response.IsSuccessStatusCode)
        {
            throw await OtaApiException.FromResponseAsync(response);
        }
    }
}

/// <summary>查询 OTA 链接失败时的业务异常。</summary>
public sealed class OtaApiException : Exception
{
    public OtaApiException(string message, int statusCode)
        : base(message)
    {
        StatusCode = statusCode;
    }

    public int StatusCode { get; }

    public static async Task<OtaApiException> FromResponseAsync(HttpResponseMessage response)
    {
        var detail = await TryReadErrorAsync(response);
        var fallback = response.StatusCode switch
        {
            System.Net.HttpStatusCode.NotFound => "未找到对应版本的 ROM。",
            System.Net.HttpStatusCode.PaymentRequired => "服务端信用点不足,无法解析下载链接。",
            System.Net.HttpStatusCode.Unauthorized => "服务端认证失败。",
            System.Net.HttpStatusCode.BadRequest => "查询参数不合法。",
            System.Net.HttpStatusCode.TooManyRequests => "请求过于频繁,请稍后再试。",
            _ => "服务端返回错误。"
        };
        return new OtaApiException(string.IsNullOrWhiteSpace(detail) ? fallback : detail, (int)response.StatusCode);
    }

    private static async Task<string?> TryReadErrorAsync(HttpResponseMessage response)
    {
        try
        {
            await using var stream = await response.Content.ReadAsStreamAsync();
            using var document = await JsonDocument.ParseAsync(stream);
            if (document.RootElement.TryGetProperty("error", out var error))
            {
                return error.GetString();
            }
        }
        catch
        {
            // 忽略响应解析失败,回退到状态码文案。
        }

        return null;
    }
}
