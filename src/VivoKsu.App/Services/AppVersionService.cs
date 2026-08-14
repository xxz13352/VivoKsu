using System.Net.Http;
using System.Net.Http.Json;
using System.Text.Json;

namespace VivoKsu.App.Services;

/// <summary>
/// VivoKsu 版本策略查询(启动强制更新拦截用)。调用免登录的公开端点
/// <c>GET /api/app/version?current=…</c>;网络异常时放行,由后续请求的 426 兜底。
/// </summary>
public sealed class AppVersionService : IDisposable
{
    private readonly HttpClient http;
    private readonly string baseUrl;

    public AppVersionService(string baseUrl = OtaApiClient.DefaultBaseUrl)
        : this(new HttpClient(), baseUrl)
    {
    }

    public AppVersionService(HttpClient http, string baseUrl = OtaApiClient.DefaultBaseUrl)
    {
        this.baseUrl = baseUrl;
        this.http = http;
        http.DefaultRequestHeaders.TryAddWithoutValidation("X-VivoKsu-Version", AppInfo.Version);
    }

    /// <summary>查询版本策略。任何失败都返回 <see cref="VersionCheckResult.AllowAll"/>(不阻塞启动)。</summary>
    public async Task<VersionCheckResult> CheckAsync(CancellationToken cancellationToken)
    {
        try
        {
            using var response = await http.GetAsync(
                $"{baseUrl}/api/app/version?current={Uri.EscapeDataString(AppInfo.Version)}",
                HttpCompletionOption.ResponseHeadersRead,
                cancellationToken).ConfigureAwait(false);
            var payload = await response.Content.ReadFromJsonAsync<JsonElement>(cancellationToken: cancellationToken).ConfigureAwait(false);
            return new VersionCheckResult(
                GetString(payload, "latest"),
                GetString(payload, "min"),
                GetString(payload, "download_url"),
                GetBool(payload, "update_required"),
                GetBool(payload, "force_update"));
        }
        catch
        {
            return VersionCheckResult.AllowAll;
        }
    }

    public void Dispose() => http.Dispose();

    private static string? GetString(JsonElement element, string name) =>
        element.TryGetProperty(name, out var value) && value.ValueKind == JsonValueKind.String ? value.GetString() : null;

    private static bool GetBool(JsonElement element, string name) =>
        element.TryGetProperty(name, out var value) && value.ValueKind == JsonValueKind.True;
}

/// <summary>版本策略查询结果。<c>ForceUpdate</c> 为真时客户端必须更新。</summary>
public sealed record VersionCheckResult(
    string? Latest,
    string? MinVersion,
    string? DownloadUrl,
    bool UpdateRequired,
    bool ForceUpdate)
{
    public static readonly VersionCheckResult AllowAll = new(null, null, null, false, false);
}
