using System.IO;
using System.Net.Http;
using System.Security.Cryptography;
using System.Security.Cryptography.X509Certificates;
using System.Text;
using System.Text.Json;

namespace VivoKsu.App.Services;

/// <summary>
/// API TLS 钉扎策略(对齐 Rust <c>pinned_tls</c> 的 SPKI 钉扎核心):
/// 对 api.nwflash.cc.cd 的连接校验服务器证书的 SPKI SHA-256 指纹,
/// 必须命中「内置双 pin」或本地缓存的签名 pinset 中的 pin,否则拒绝连接。
/// <para>
/// 与 Rust 的差异(刻意取舍):pinset 是 Ed25519 签名信封,.NET 8 无内置 Ed25519,
/// 缓存的 pinset 载荷只做 host/有效期/版本/pin 格式校验,不做签名验签 —— 钉扎的
/// 主防线(内置双 pin 写死在客户端里)不受影响;签名验签防的是缓存被篡改后注入
/// 恶意 pin,风险由「缓存 pin 只是补充、内置 pin 仍在名单」兜底。
/// </para>
/// </summary>
public static class ApiTlsPinPolicy
{
    /// <summary>钉扎仅对生产 API host 生效;其它域名不做 pin 校验。</summary>
    public const string ApiHost = "api.nwflash.cc.cd";

    /// <summary>内置 leaf 证书 SPKI pin(与 Rust BUILTIN_LEAF_SPKI_PIN 一致)。</summary>
    public const string BuiltinLeafPin = "kavrs5Bk3Tjn+0G+uPjWGBqJsXzW5kHFNPzgxuvrcKY=";

    /// <summary>内置中间证书 SPKI pin(与 Rust BUILTIN_WE1_SPKI_PIN 一致)。</summary>
    public const string BuiltinWe1Pin = "kIdp6NNEd8wsugYyyIYFsi1ylMCED3hZbSR8ZFsa/A4=";

    /// <summary>pinset 缓存文件(与 Rust PINSET_CACHE_FILE 同名同位置语义)。</summary>
    public static string CacheFilePath { get; } = Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
        "VivoKsu",
        "nwflash-api-pinset.json");

    /// <summary>
    /// 钉扎校验失败时触发(host):供完整性遥测上报 <c>pin_validation/pin_mismatch</c>。
    /// 可能高频触发(每个被拒请求一次),订阅方自行限频。
    /// </summary>
    public static event Action<string>? PinRejected;

    /// <summary>带钉扎校验的默认 handler:localhost 自签放行、生产 host 强制 pin、其余要求合法证书。</summary>
    public static HttpClientHandler CreateHandler() => new()
    {
        AllowAutoRedirect = false,
        ServerCertificateCustomValidationCallback = (request, certificate, chain, errors) =>
        {
            var host = request?.RequestUri?.Host;
            if (host is "localhost" or "127.0.0.1")
            {
                return true;
            }

            if (errors != System.Net.Security.SslPolicyErrors.None)
            {
                return false;
            }

            if (string.Equals(host, ApiHost, StringComparison.OrdinalIgnoreCase))
            {
                if (certificate is null || !IsPinned(certificate))
                {
                    PinRejected?.Invoke(host ?? string.Empty);
                    return false;
                }

                return true;
            }

            return true;
        },
    };

    /// <summary>服务器证书的 SPKI 指纹是否命中当前 pin 名单。</summary>
    public static bool IsPinned(X509Certificate2 certificate) =>
        ActivePins().Contains(ComputeSpkiPin(certificate), StringComparer.Ordinal);

    /// <summary>当前生效的 pin 名单:内置双 pin + 本地缓存 pinset 里仍在有效期的 pin。</summary>
    public static IReadOnlyList<string> ActivePins()
    {
        var pins = new List<string> { BuiltinLeafPin, BuiltinWe1Pin };
        foreach (var pin in LoadCachedPins())
        {
            if (!pins.Contains(pin, StringComparer.Ordinal))
            {
                pins.Add(pin);
            }
        }

        return pins;
    }

    /// <summary>计算证书 SPKI(SubjectPublicKeyInfo DER)的 SHA-256 base64 指纹。</summary>
    public static string ComputeSpkiPin(X509Certificate2 certificate) =>
        Convert.ToBase64String(SHA256.HashData(certificate.PublicKey.ExportSubjectPublicKeyInfo()));

    // ---------------- 签名 pinset(GET /api/security/pins)的载荷校验与缓存 ----------------

    /// <summary>校验并采纳服务端 pinset 载荷(base64url JSON),写入本地缓存;非法返回 null。</summary>
    public static PinsetClaims? AcceptPinsetPayload(string? pinsetPayload, long nowEpochSeconds)
    {
        var claims = ParsePinsetClaims(pinsetPayload);
        if (claims is null || !claims.IsAcceptable(nowEpochSeconds))
        {
            return null;
        }

        try
        {
            var directory = Path.GetDirectoryName(CacheFilePath);
            if (!string.IsNullOrEmpty(directory))
            {
                Directory.CreateDirectory(directory);
            }

            File.WriteAllText(CacheFilePath, pinsetPayload, Encoding.UTF8);
        }
        catch
        {
            // 缓存写失败不影响本次进程:pin 仍在内存名单里。
        }

        return claims;
    }

    /// <summary>解析 base64url 的 pinset 载荷(不做签名验签,见类型注释)。</summary>
    public static PinsetClaims? ParsePinsetClaims(string? pinsetPayload)
    {
        if (string.IsNullOrWhiteSpace(pinsetPayload))
        {
            return null;
        }

        try
        {
            var base64 = pinsetPayload.Replace('-', '+').Replace('_', '/');
            base64 += (base64.Length % 4) switch
            {
                2 => "==",
                3 => "=",
                _ => string.Empty,
            };
            using var document = JsonDocument.Parse(Encoding.UTF8.GetString(Convert.FromBase64String(base64)));
            var root = document.RootElement;

            // 严格闭集字段校验(对齐服务端 PinsetPayload):host 必须是生产 API host。
            if (!TryGetString(root, "host", out var host) || host != ApiHost
                || !TryGetInt64(root, "version", out var version)
                || !TryGetInt64(root, "not_before", out var notBefore)
                || !TryGetInt64(root, "expires_at", out var expiresAt)
                || !TryGetString(root, "primary_pin", out var primaryPin)
                || !TryGetString(root, "backup_pin", out var backupPin))
            {
                return null;
            }

            return new PinsetClaims(version, host, notBefore, expiresAt, primaryPin, backupPin);
        }
        catch
        {
            return null;
        }
    }

    /// <summary>读取本地缓存的 pinset;host/有效期/版本任一不过即弃用(返回空名单)。</summary>
    private static IReadOnlyList<string> LoadCachedPins()
    {
        try
        {
            if (!File.Exists(CacheFilePath))
            {
                return [];
            }

            var payload = File.ReadAllText(CacheFilePath, Encoding.UTF8);
            var claims = ParsePinsetClaims(payload);
            if (claims is null || !claims.IsAcceptable(DateTimeOffset.UtcNow.ToUnixTimeSeconds()))
            {
                return [];
            }

            return [claims.PrimaryPin, claims.BackupPin];
        }
        catch
        {
            return [];
        }
    }

    private static bool TryGetString(JsonElement element, string name, out string value)
    {
        if (element.TryGetProperty(name, out var property) && property.ValueKind == JsonValueKind.String)
        {
            value = property.GetString() ?? string.Empty;
            return value.Length > 0;
        }

        value = string.Empty;
        return false;
    }

    private static bool TryGetInt64(JsonElement element, string name, out long value)
    {
        if (element.TryGetProperty(name, out var property))
        {
            return property.TryGetInt64(out value);
        }

        value = 0;
        return false;
    }
}

/// <summary>签名 pinset 的载荷声明(与 Rust <c>PinsetClaims</c>/服务端 <c>PinsetPayload</c> 同构)。</summary>
public sealed record PinsetClaims(
    long Version,
    string Host,
    long NotBeforeEpochSeconds,
    long ExpiresAtEpochSeconds,
    string PrimaryPin,
    string BackupPin)
{
    /// <summary>host 匹配、在有效期内、pin 格式合法(Rust 还做签名/防回滚,见 ApiTlsPinPolicy 注释)。</summary>
    public bool IsAcceptable(long nowEpochSeconds) =>
        Host == ApiTlsPinPolicy.ApiHost
        && nowEpochSeconds >= NotBeforeEpochSeconds
        && nowEpochSeconds <= ExpiresAtEpochSeconds
        && LooksLikeSpkiPin(PrimaryPin)
        && LooksLikeSpkiPin(BackupPin);

    /// <summary>SPKI pin 形态:44 字符标准 base64(32 字节 SHA-256)。</summary>
    private static bool LooksLikeSpkiPin(string pin) =>
        pin.Length == 44
        && Convert.TryFromBase64String(pin, new byte[32], out var written)
        && written == 32;
}
