using System.Net.Http;
using System.Net.Http.Json;
using System.Text.Json;
using System.Text.RegularExpressions;
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
        http.DefaultRequestHeaders.TryAddWithoutValidation("X-Nwflash-Version", AppInfo.Version);
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
        // 统一走钉扎策略:localhost 自签放行;api.nwflash.cc.cd 强制 SPKI pin 校验
        // (对齐 Rust pinned_tls);其它域名要求合法证书;一律不跟随重定向。
        return new HttpClient(ApiTlsPinPolicy.CreateHandler())
        {
            // 与 Rust 客户端一致:30s 整体超时,避免接受连接但不返回的请求永久挂住轮询循环。
            Timeout = TimeSpan.FromSeconds(30),
        };
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
        await EnsureSuccessAsync(response, cancellationToken).ConfigureAwait(false);

        var rom = await response.Content.ReadFromJsonAsync<RomInfo>(cancellationToken: cancellationToken);
        if (rom is null || string.IsNullOrWhiteSpace(rom.Url))
        {
            throw new OtaApiException("服务端返回了无效的 ROM 记录。", (int)response.StatusCode);
        }

        return rom;
    }

    /// <summary>
    /// 在线会话心跳:保持本实例「在线」并可接收服务端指令(强制下线 / 封禁 / 强制更新)。
    /// <paramref name="active"/>=false 为 goodbye,服务端删除会话行(goodbye 只校验 session_id,
    /// 忽略 <paramref name="sequence"/>,传 0 即可)。
    /// 活动心跳必须携带与登录一致的完整绑定(client_version/build_id/process_nonce)和当前租约序号,
    /// 服务端 CAS 匹配后将序号 +1;响应中的 lease_payload 含新序号,供调用方同步。
    /// </summary>
    public async Task<HeartbeatResult> HeartbeatAsync(string sessionId, long sequence, bool active, CancellationToken cancellationToken)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(sessionId);

        object body = active
            ? new
            {
                session_id = sessionId,
                client_version = AppInfo.Version,
                build_id = ClientSession.BuildId,
                process_nonce = ClientSession.ProcessNonce,
                sequence,
                active,
            }
            : new { session_id = sessionId, active };

        using var response = await http.PostAsJsonAsync(
            $"{BaseUrl}/api/heartbeat",
            body,
            cancellationToken).ConfigureAwait(false);

        await EnsureSuccessAsync(response, cancellationToken).ConfigureAwait(false);

        var parsed = await response.Content.ReadFromJsonAsync<JsonElement>(cancellationToken: cancellationToken).ConfigureAwait(false);
        var forceExit = parsed.TryGetProperty("force_exit", out var fe)
            && fe.ValueKind == JsonValueKind.True;
        var reason = parsed.TryGetProperty("reason", out var r)
            ? r.GetString()
            : null;
        return new HeartbeatResult(forceExit, reason, ParseLeaseSequence(parsed));
    }

    /// <summary>解析活动心跳响应里的 lease_payload(unpadded base64url JSON)中的最新租约序号;解析失败返回 null。</summary>
    private static long? ParseLeaseSequence(JsonElement parsed)
    {
        try
        {
            if (!parsed.TryGetProperty("lease_payload", out var payload) || payload.ValueKind != JsonValueKind.String)
            {
                return null;
            }

            // .NET 8 无 Base64Url:先转回标准 base64(补齐 padding)再解码。
            var base64 = payload.GetString()!.Replace('-', '+').Replace('_', '/');
            base64 += (base64.Length % 4) switch
            {
                2 => "==",
                3 => "=",
                _ => string.Empty,
            };
            var claims = System.Text.Encoding.UTF8.GetString(Convert.FromBase64String(base64));
            using var document = JsonDocument.Parse(claims);
            return document.RootElement.TryGetProperty("sequence", out var sequence) && sequence.TryGetInt64(out var value)
                ? value
                : null;
        }
        catch
        {
            // 租约载荷缺失/损坏不影响心跳主流程:调用方按本地递增处理。
            return null;
        }
    }

    /// <summary>服务健康探活(GET /health):返回 (status, source);用于设置页连通性诊断。</summary>
    public async Task<(string Status, string Source)> CheckHealthAsync(CancellationToken cancellationToken)
    {
        using var response = await http.GetAsync($"{BaseUrl}/health", cancellationToken).ConfigureAwait(false);
        if (!response.IsSuccessStatusCode)
        {
            throw await OtaApiException.FromResponseAsync(response).ConfigureAwait(false);
        }

        var body = await response.Content.ReadFromJsonAsync<JsonElement>(cancellationToken: cancellationToken).ConfigureAwait(false);
        var status = body.TryGetProperty("status", out var s) && s.ValueKind == JsonValueKind.String
            ? s.GetString() ?? string.Empty
            : string.Empty;
        var source = body.TryGetProperty("source", out var src) && src.ValueKind == JsonValueKind.String
            ? src.GetString() ?? string.Empty
            : string.Empty;
        return (status, source);
    }

    /// <summary>
    /// 拉取签名 pinset(GET /api/security/pins)并写入本地缓存:校验 host/有效期/pin 格式后,
    /// 其中的 pin 会加入后续所有请求的钉扎名单(见 <see cref="ApiTlsPinPolicy"/>)。
    /// 返回是否成功采纳(载荷非法/过期返回 false,内置双 pin 不受影响)。
    /// </summary>
    public async Task<bool> RefreshPinsetAsync(CancellationToken cancellationToken)
    {
        using var response = await http.GetAsync($"{BaseUrl}/api/security/pins", cancellationToken).ConfigureAwait(false);
        if (!response.IsSuccessStatusCode)
        {
            return false;
        }

        var body = await response.Content.ReadFromJsonAsync<JsonElement>(cancellationToken: cancellationToken).ConfigureAwait(false);
        var payload = body.TryGetProperty("pinset_payload", out var p) && p.ValueKind == JsonValueKind.String
            ? p.GetString()
            : null;
        return ApiTlsPinPolicy.AcceptPinsetPayload(payload, DateTimeOffset.UtcNow.ToUnixTimeSeconds()) is not null;
    }

    /// <summary>
    /// 统一的状态码处理(对齐 Rust <c>handle_api_error</c>):426 → 强制更新异常,
    /// 其它非 2xx → 业务异常(带服务端 <c>error</c> 文案或状态码兜底文案)。
    /// </summary>
    private static async Task EnsureSuccessAsync(HttpResponseMessage response, CancellationToken cancellationToken)
    {
        // 先判状态码再解析 body:空/非 JSON 响应体(如 WAF HTML 403)不得被当作网络抖动静默吞掉。
        if (response.StatusCode == System.Net.HttpStatusCode.UpgradeRequired)
        {
            var update = await response.Content.ReadFromJsonAsync<JsonElement>(cancellationToken: cancellationToken).ConfigureAwait(false);
            throw UpdateRequiredException.FromResponse(update);
        }

        if (!response.IsSuccessStatusCode)
        {
            throw await OtaApiException.FromResponseAsync(response).ConfigureAwait(false);
        }
    }

    /// <summary>
    /// 完整性遥测上报(对齐 Rust <c>report_integrity</c>):<c>POST /api/integrity/report</c>。
    /// 服务端为严格闭集字段 + IP 窗口限流(60s/20 条)+ event_id 幂等;本地先做同样的
    /// 长度/字符集校验,非法直接抛 <see cref="ArgumentException"/>(不浪费一次网络往返)。
    /// 上报失败由调用方按 best-effort 处理(不得阻塞启动或登录)。
    /// </summary>
    public async Task ReportIntegrityAsync(IntegrityReportRequest report, CancellationToken cancellationToken)
    {
        ArgumentNullException.ThrowIfNull(report);
        ValidateIdentifier(report.EventId, 64, nameof(report.EventId));
        ValidateClientVersion(report.ClientVersion, nameof(report.ClientVersion));
        ValidateIdentifier(report.BuildId, 128, nameof(report.BuildId));
        ValidateEpochSeconds(report.OccurredAtEpochSeconds, nameof(report.OccurredAtEpochSeconds));

        using var response = await http.PostAsJsonAsync(
            $"{BaseUrl}/api/integrity/report",
            new
            {
                event_id = report.EventId,
                phase = ToSnakeCase(report.Phase),
                reason = ToSnakeCase(report.Reason),
                client_version = report.ClientVersion,
                build_id = report.BuildId,
                occurred_at = report.OccurredAtEpochSeconds,
            },
            cancellationToken).ConfigureAwait(false);

        await EnsureSuccessAsync(response, cancellationToken).ConfigureAwait(false);
    }

    /// <summary>
    /// 崩溃报告补传(对齐 Rust <c>upload_crash_report</c>):<c>POST /api/diagnostics/crash</c>。
    /// 匿名(未登录)也可上报;202 首次接受 / 200 重复幂等,都视为成功(429 窗口配额已满会抛异常)。
    /// 调用方必须在构造前完成价值过滤:凭据、URL userinfo、私钥等内容不得上传。
    /// </summary>
    public async Task UploadCrashReportAsync(CrashReportRequest report, CancellationToken cancellationToken)
    {
        ArgumentNullException.ThrowIfNull(report);
        ValidateIdentifier(report.EventId, 64, nameof(report.EventId));
        ValidateClientVersion(report.ClientVersion, nameof(report.ClientVersion));
        ValidateIdentifier(report.BuildId, 128, nameof(report.BuildId));
        ValidateIdentifier(report.SessionId, 64, nameof(report.SessionId));
        ValidateEpochSeconds(report.OccurredAtEpochSeconds, nameof(report.OccurredAtEpochSeconds));

        var panic = report.PanicMessage;
        if (panic.Length is 0 || System.Text.Encoding.UTF8.GetByteCount(panic) > CrashReportRequest.MaxPanicMessageBytes)
        {
            throw new ArgumentException("崩溃信息为空或超出长度上限。", nameof(report));
        }

        var backtrace = report.Backtrace ?? string.Empty;
        if (System.Text.Encoding.UTF8.GetByteCount(backtrace) > CrashReportRequest.MaxBacktraceBytes)
        {
            throw new ArgumentException("调用栈超出长度上限。", nameof(report));
        }

        using var response = await http.PostAsJsonAsync(
            $"{BaseUrl}/api/diagnostics/crash",
            new
            {
                event_id = report.EventId,
                client_version = report.ClientVersion,
                build_id = report.BuildId,
                session_id = report.SessionId,
                panic_message = panic,
                backtrace,
                occurred_at = report.OccurredAtEpochSeconds,
            },
            cancellationToken).ConfigureAwait(false);

        await EnsureSuccessAsync(response, cancellationToken).ConfigureAwait(false);
    }

    /// <summary>JSON 线格式:枚举名转 snake_case(对齐 Rust <c>#[serde(rename_all = "snake_case")]</c>)。</summary>
    private static string ToSnakeCase<TEnum>(TEnum value)
        where TEnum : struct, Enum
    {
        var name = value.ToString();
        return string.Concat(name.Select((c, i) => i > 0 && char.IsUpper(c) ? "_" + char.ToLowerInvariant(c) : char.ToLowerInvariant(c).ToString()));
    }

    /// <summary>校验服务端标识字段:<c>[A-Za-z0-9._:-]{1,max}</c>(对齐 Rust <c>is_identifier</c>)。</summary>
    private static void ValidateIdentifier(string value, int maxLength, string parameterName)
    {
        if (string.IsNullOrEmpty(value)
            || value.Length > maxLength
            || !Regex.IsMatch(value, "^[A-Za-z0-9._:-]+$"))
        {
            throw new ArgumentException($"标识字段非法:仅允许 1-{maxLength} 位 A-Za-z0-9._:- 。", parameterName);
        }
    }

    /// <summary>校验客户端版本号:<c>[A-Za-z0-9][A-Za-z0-9._+-]{0,31}</c>(对齐 Rust <c>is_client_version</c>)。</summary>
    private static void ValidateClientVersion(string value, string parameterName)
    {
        if (string.IsNullOrEmpty(value)
            || value.Length > 32
            || !Regex.IsMatch(value, "^[A-Za-z0-9][A-Za-z0-9._+-]*$"))
        {
            throw new ArgumentException("客户端版本号非法:1-32 位,首字符字母数字。", parameterName);
        }
    }

    /// <summary>校验 Unix 秒时间戳落在服务端安全整数区间(对齐 Rust 的 1..=2^53-1 校验)。</summary>
    private static void ValidateEpochSeconds(long value, string parameterName)
    {
        if (value is < 1 or > 9_007_199_254_740_991)
        {
            throw new ArgumentOutOfRangeException(parameterName, "时间戳必须为正的安全整数(Unix 秒)。");
        }
    }

    /// <summary>查询在线用户列表(显示名/版本/时长,不含 username/IP)。</summary>
    public async Task<IReadOnlyList<OnlineSession>> GetOnlineAsync(CancellationToken cancellationToken)
    {
        using var response = await http.GetAsync($"{BaseUrl}/api/online", cancellationToken).ConfigureAwait(false);
        await EnsureSuccessAsync(response, cancellationToken).ConfigureAwait(false);

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
        await EnsureSuccessAsync(response, cancellationToken).ConfigureAwait(false);

        var body = await response.Content.ReadFromJsonAsync<JsonElement>(cancellationToken: cancellationToken).ConfigureAwait(false);
        var allowed = body.TryGetProperty("allowed", out var value)
            && value.ValueKind == JsonValueKind.True;
        var reason = body.TryGetProperty("reason", out var r)
            ? r.GetString()
            : null;
        return allowed ? OperationAuthorization.Allow() : OperationAuthorization.Deny(reason ?? "服务端未许可此操作。");
    }

    /// <summary>批量上传使用日志(服务端按操作分类存储,绑定当前 token 用户)。失败抛异常,调用方按 best-effort 处理。</summary>
    public async Task<UsageLogUploadResult> UploadUsageLogsAsync(IReadOnlyList<UsageLogEntry> logs, CancellationToken cancellationToken)
    {
        if (logs.Count == 0)
        {
            return new UsageLogUploadResult(true, null);
        }

        using var response = await http.PostAsJsonAsync(
            $"{BaseUrl}/api/usage/logs",
            new { logs },
            cancellationToken).ConfigureAwait(false);
        await EnsureSuccessAsync(response, cancellationToken).ConfigureAwait(false);

        // 与 Rust UsageLogUploadResponse 一致:{ ok, received }。响应缺失时按成功处理(写入即成功)。
        var body = await response.Content.ReadFromJsonAsync<JsonElement>(cancellationToken: cancellationToken).ConfigureAwait(false);
        var ok = !body.TryGetProperty("ok", out var okValue) || okValue.ValueKind != JsonValueKind.False;
        var received = body.TryGetProperty("received", out var receivedValue) && receivedValue.TryGetInt64(out var count)
            ? count
            : (long?)null;
        return new UsageLogUploadResult(ok, received);
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
        // 与 Rust CloudflareError::user_message 保持一致的状态码兜底文案。
        var fallback = response.StatusCode switch
        {
            System.Net.HttpStatusCode.NotFound => "未找到对应版本的 ROM。",
            System.Net.HttpStatusCode.PaymentRequired => "服务端信用点不足,无法解析下载链接。",
            System.Net.HttpStatusCode.Unauthorized => "服务端认证失败。",
            System.Net.HttpStatusCode.BadRequest => "查询参数不合法。",
            System.Net.HttpStatusCode.Forbidden => "服务端拒绝了当前请求。",
            System.Net.HttpStatusCode.Conflict => "会话已失效或发生冲突,请重新登录。",
            System.Net.HttpStatusCode.Gone => "登录已过期(长时间离线或睡眠),请重新登录。",
            System.Net.HttpStatusCode.TooManyRequests => "请求过于频繁,请稍后再试。",
            >= System.Net.HttpStatusCode.InternalServerError => "服务暂时不可用,请稍后重试。",
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
