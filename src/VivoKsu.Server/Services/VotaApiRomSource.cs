using System.Net.Http.Headers;
using System.Net.Http.Json;
using System.Text.Json;
using VivoKsu.Server.Models;

namespace VivoKsu.Server.Services;

/// <summary>
/// 从 VOTA API(https://api.otau.cc.cd)解析 OTA 下载链接。
/// 默认调用 resolve_url(OTA 全量包,-1 信用点),可配置 resolve_flash_url(线刷包,-3)
/// 或 dev_resolve(设备端,用 device_id 鉴权)。
/// 鉴权优先 API Token(Authorization: Bearer),其次 user/pass,再次 device_id。
/// </summary>
public sealed class VotaApiRomSource : IRomSource
{
    private readonly HttpClient http;
    private readonly VotaApiOptions options;

    public VotaApiRomSource(HttpClient http, VotaApiOptions options)
    {
        this.http = http;
        this.options = options;
    }

    public async Task<RomInfo?> ResolveAsync(string pd, string version, CancellationToken cancellationToken)
    {
        var requestUri = new UriBuilder(options.BaseUrl) { Query = $"action={Uri.EscapeDataString(options.Action)}" }.Uri;
        using var request = new HttpRequestMessage(HttpMethod.Post, requestUri);
        if (options.UseApiToken)
        {
            request.Headers.Authorization = new AuthenticationHeaderValue("Bearer", options.ApiToken);
        }

        request.Content = JsonContent.Create(BuildRequestBody(pd, version));
        using var response = await http.SendAsync(request, HttpCompletionOption.ResponseHeadersRead, cancellationToken);
        var payload = await response.Content.ReadFromJsonAsync<JsonElement>(cancellationToken: cancellationToken);

        if (!payload.TryGetProperty("ok", out var ok) || !ok.GetBoolean())
        {
            var code = payload.TryGetProperty("code", out var codeValue) ? codeValue.GetString() : null;
            var error = payload.TryGetProperty("error", out var errorValue) ? errorValue.GetString() : null;
            throw new RomResolveException(error ?? "VOTA 未能解析 ROM 下载链接。", code);
        }

        var link = payload.TryGetProperty("url", out var urlValue) ? urlValue.GetString() : null;
        if (string.IsNullOrWhiteSpace(link))
        {
            throw new RomResolveException("VOTA 响应缺少 url 字段。");
        }

        return new RomInfo(
            payload.TryGetProperty("pd", out var pdValue) && pdValue.ValueKind == JsonValueKind.String ? pdValue.GetString()! : pd,
            payload.TryGetProperty("version", out var versionValue) && versionValue.ValueKind == JsonValueKind.String ? versionValue.GetString()! : version,
            link);
    }

    private object BuildRequestBody(string pd, string version)
    {
        if (options.UseApiToken)
        {
            // token 鉴权忽略 body.user,由平台按 token 归属扣信用点。
            return new { ver = options.Ver, pd, version };
        }

        if (options.UseDeviceAuth)
        {
            return new { device_id = options.DeviceId, pd, version };
        }

        return new { user = options.User, pass = options.Pass, ver = options.Ver, pd, version };
    }
}
