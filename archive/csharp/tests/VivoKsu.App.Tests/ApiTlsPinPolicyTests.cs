using System.Security.Cryptography;
using System.Security.Cryptography.X509Certificates;
using System.Text;
using System.Text.Json;
using FluentAssertions;
using VivoKsu.App.Services;

namespace VivoKsu.App.Tests;

public class ApiTlsPinPolicyTests : IDisposable
{
    /// <summary>pinset 缓存是静态固定路径:测试前备份、测试后恢复,避免污染生产缓存。</summary>
    private readonly string? backupCache;
    private readonly bool hadCache;

    public ApiTlsPinPolicyTests()
    {
        hadCache = File.Exists(ApiTlsPinPolicy.CacheFilePath);
        if (hadCache)
        {
            backupCache = File.ReadAllText(ApiTlsPinPolicy.CacheFilePath);
            File.Delete(ApiTlsPinPolicy.CacheFilePath);
        }
    }

    public void Dispose()
    {
        try
        {
            if (hadCache && backupCache is not null)
            {
                File.WriteAllText(ApiTlsPinPolicy.CacheFilePath, backupCache);
            }
            else if (File.Exists(ApiTlsPinPolicy.CacheFilePath))
            {
                File.Delete(ApiTlsPinPolicy.CacheFilePath);
            }
        }
        catch
        {
            // 恢复失败只影响本机缓存,内置双 pin 不受影响。
        }
    }

    [Fact]
    public void ComputeSpkiPin_is_the_sha256_of_the_subject_public_key_info()
    {
        using var certificate = CreateSelfSignedCertificate();

        var pin = ApiTlsPinPolicy.ComputeSpkiPin(certificate);

        // 独立复算:PublicKey.ExportSubjectPublicKeyInfo() 即 SPKI DER,SHA-256 后标准 base64。
        var expected = Convert.ToBase64String(SHA256.HashData(certificate.PublicKey.ExportSubjectPublicKeyInfo()));
        pin.Should().Be(expected);
        pin.Should().HaveLength(44);
    }

    [Fact]
    public void IsPinned_accepts_a_certificate_whose_pin_is_in_the_cached_pinset()
    {
        using var certificate = CreateSelfSignedCertificate();
        var pin = ApiTlsPinPolicy.ComputeSpkiPin(certificate);
        var now = DateTimeOffset.UtcNow.ToUnixTimeSeconds();

        var payload = EncodePinset(new
        {
            version = 1L,
            host = ApiTlsPinPolicy.ApiHost,
            not_before = now - 60,
            expires_at = now + 3600,
            primary_pin = pin,
            backup_pin = ApiTlsPinPolicy.BuiltinWe1Pin,
        });

        ApiTlsPinPolicy.AcceptPinsetPayload(payload, now).Should().NotBeNull();
        ApiTlsPinPolicy.IsPinned(certificate).Should().BeTrue();
        File.Exists(ApiTlsPinPolicy.CacheFilePath).Should().BeTrue();
    }

    [Fact]
    public void IsPinned_rejects_an_unknown_certificate_when_no_cache_exists()
    {
        using var certificate = CreateSelfSignedCertificate();

        ApiTlsPinPolicy.IsPinned(certificate).Should().BeFalse();
    }

    [Fact]
    public void ParsePinsetClaims_rejects_wrong_host_expired_window_and_bad_pins()
    {
        var now = DateTimeOffset.UtcNow.ToUnixTimeSeconds();
        const string goodPin = ApiTlsPinPolicy.BuiltinLeafPin;

        ApiTlsPinPolicy.ParsePinsetClaims(EncodePinset(new
        {
            version = 1L,
            host = "evil.example.com", // host 不匹配
            not_before = now - 60,
            expires_at = now + 3600,
            primary_pin = goodPin,
            backup_pin = goodPin,
        })).Should().BeNull();

        ApiTlsPinPolicy.ParsePinsetClaims(EncodePinset(new
        {
            version = 1L,
            host = ApiTlsPinPolicy.ApiHost,
            not_before = now - 3600,
            expires_at = now - 60, // 已过期
            primary_pin = goodPin,
            backup_pin = goodPin,
        }))!.IsAcceptable(now).Should().BeFalse();

        ApiTlsPinPolicy.ParsePinsetClaims(EncodePinset(new
        {
            version = 1L,
            host = ApiTlsPinPolicy.ApiHost,
            not_before = now - 60,
            expires_at = now + 3600,
            primary_pin = "not-a-pin", // pin 格式非法
            backup_pin = goodPin,
        }))!.IsAcceptable(now).Should().BeFalse();

        ApiTlsPinPolicy.ParsePinsetClaims("not-base64url!!").Should().BeNull();
        ApiTlsPinPolicy.ParsePinsetClaims(null).Should().BeNull();
    }

    [Fact]
    public void ActivePins_always_contains_the_two_builtin_pins()
    {
        var pins = ApiTlsPinPolicy.ActivePins();

        pins.Should().Contain(ApiTlsPinPolicy.BuiltinLeafPin).And.Contain(ApiTlsPinPolicy.BuiltinWe1Pin);
    }

    private static X509Certificate2 CreateSelfSignedCertificate()
    {
        using var rsa = RSA.Create(2048);
        var request = new CertificateRequest("CN=pin-test", rsa, HashAlgorithmName.SHA256, RSASignaturePadding.Pkcs1);
        return request.CreateSelfSigned(DateTimeOffset.UtcNow.AddMinutes(-5), DateTimeOffset.UtcNow.AddHours(1));
    }

    private static string EncodePinset(object payload)
    {
        var json = JsonSerializer.Serialize(payload);
        return Convert.ToBase64String(Encoding.UTF8.GetBytes(json))
            .Replace('+', '-')
            .Replace('/', '_')
            .TrimEnd('=');
    }
}
