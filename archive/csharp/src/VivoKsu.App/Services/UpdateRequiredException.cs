using System.Text.Json;

namespace VivoKsu.App.Services;

/// <summary>
/// 服务端要求强制更新(HTTP 426 <c>UPDATE_REQUIRED</c>)。由 <see cref="App"/> 捕获并弹强制更新窗。
/// </summary>
public sealed class UpdateRequiredException : Exception
{
    public UpdateRequiredException(string message, string? latest, string? minVersion, string? downloadUrl)
        : base(message)
    {
        Latest = latest;
        MinVersion = minVersion;
        DownloadUrl = downloadUrl;
    }

    /// <summary>服务端最新版本号。</summary>
    public string? Latest { get; }

    /// <summary>服务端允许的最低版本号。</summary>
    public string? MinVersion { get; }

    /// <summary>新版本下载链接(可能未配置)。</summary>
    public string? DownloadUrl { get; }

    /// <summary>从 426 响应 JSON(<c>{ error, code, latest, min, download_url }</c>)解析。</summary>
    public static UpdateRequiredException FromResponse(
        JsonElement payload,
        string fallbackMessage = "需要更新 VivoKsu 后才能继续使用。")
    {
        string? Get(string name) =>
            payload.TryGetProperty(name, out var value) && value.ValueKind == JsonValueKind.String
                ? value.GetString()
                : null;
        return new UpdateRequiredException(
            Get("error") ?? fallbackMessage,
            Get("latest"),
            Get("min"),
            Get("download_url"));
    }
}
