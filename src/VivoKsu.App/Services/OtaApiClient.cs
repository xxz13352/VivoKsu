using System.Net.Http;
using System.Net.Http.Json;
using System.Text.Json;
using VivoKsu.App.Models;

namespace VivoKsu.App.Services;

/// <summary>
/// 查询本地 VivoKsu.Server,按 PD + 版本号获取 OTA 下载链接。
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
    }

    /// <summary>默认服务端地址:Cloudflare Worker 上的 VivoKsu ROM 代理(域名 nwflash.cc.cd)。</summary>
    public const string DefaultBaseUrl = "https://api.nwflash.cc.cd";

    /// <summary>服务端基地址,页面允许用户修改。</summary>
    public string BaseUrl { get; set; }

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
