using System.Text.Json;
using FluentAssertions;
using VivoKsu.Server.Services;

namespace VivoKsu.Server.Tests;

public class VotaApiRomSourceTests
{
    [Fact]
    public async Task ResolveAsync_posts_credentials_and_pd_version_then_maps_the_url()
    {
        StubHttpMessageHandler.Request? captured = null;
        var source = new VotaApiRomSource(
            new HttpClient(new StubHttpMessageHandler(
                """{"ok": true, "pd": "PD2417", "version": "16.2.10.0", "url": "https://sysuptxdl.vivo.com.cn/upgrade/full.zip"}""",
                req => captured = req)),
            new VotaApiOptions { BaseUrl = "https://api.otau.cc.cd", User = "testuser", Pass = "secret", Ver = "2.0.0" });

        var rom = await source.ResolveAsync("PD2417", "16.2.10.0", CancellationToken.None);

        captured.Should().NotBeNull();
        captured!.Uri.Should().StartWith("https://api.otau.cc.cd/?action=resolve_url");
        captured.Body.GetProperty("user").GetString().Should().Be("testuser");
        captured.Body.GetProperty("pass").GetString().Should().Be("secret");
        captured.Body.GetProperty("ver").GetString().Should().Be("2.0.0");
        captured.Body.GetProperty("pd").GetString().Should().Be("PD2417");
        captured.Body.GetProperty("version").GetString().Should().Be("16.2.10.0");
        rom.Should().NotBeNull();
        rom!.Pd.Should().Be("PD2417");
        rom.Version.Should().Be("16.2.10.0");
        rom.Url.Should().Be("https://sysuptxdl.vivo.com.cn/upgrade/full.zip");
    }

    [Fact]
    public async Task ResolveAsync_uses_device_id_auth_when_configured()
    {
        StubHttpMessageHandler.Request? captured = null;
        var source = new VotaApiRomSource(
            new HttpClient(new StubHttpMessageHandler(
                """{"ok": true, "pd": "PD2417", "version": "16.2.10.0", "url": "https://x/y.zip"}""",
                req => captured = req)),
            new VotaApiOptions { BaseUrl = "https://api.otau.cc.cd", DeviceId = "a".PadRight(64, '0'), Action = "dev_resolve" });

        await source.ResolveAsync("PD2417", "16.2.10.0", CancellationToken.None);

        captured!.Uri.Should().Contain("action=dev_resolve");
        captured.Body.GetProperty("device_id").GetString().Should().Be("a".PadRight(64, '0'));
        captured.Body.TryGetProperty("user", out _).Should().BeFalse();
    }

    [Fact]
    public async Task ResolveAsync_uses_api_token_auth_when_configured()
    {
        StubHttpMessageHandler.Request? captured = null;
        var source = new VotaApiRomSource(
            new HttpClient(new StubHttpMessageHandler(
                """{"ok": true, "pd": "PD2057", "version": "1.16.32", "url": "https://sysuptxdl.vivo.com.cn/full.zip"}""",
                req => captured = req)),
            new VotaApiOptions { BaseUrl = "https://api.otau.cc.cd", ApiToken = "vota_automation123" });

        await source.ResolveAsync("PD2057", "1.16.32", CancellationToken.None);

        captured!.Authorization.Should().Be("Bearer vota_automation123");
        captured.Body.GetProperty("pd").GetString().Should().Be("PD2057");
        captured.Body.GetProperty("version").GetString().Should().Be("1.16.32");
        captured.Body.TryGetProperty("user", out _).Should().BeFalse();
        captured.Body.TryGetProperty("pass", out _).Should().BeFalse();
    }

    [Fact]
    public async Task ResolveAsync_throws_with_the_vota_error_code_when_ok_is_false()
    {
        var source = new VotaApiRomSource(
            new HttpClient(new StubHttpMessageHandler("""{"ok": false, "code": "INSUFFICIENT_CREDITS"}""")),
            new VotaApiOptions { BaseUrl = "https://api.otau.cc.cd", User = "u", Pass = "p" });

        var act = () => source.ResolveAsync("PD2417", "16.2.10.0", CancellationToken.None);

        var exception = await act.Should().ThrowAsync<RomResolveException>();
        exception.Which.ErrorCode.Should().Be("INSUFFICIENT_CREDITS");
    }

    private sealed class StubHttpMessageHandler : HttpMessageHandler
    {
        private readonly string responseJson;
        private readonly Action<Request>? onRequest;

        public StubHttpMessageHandler(string responseJson, Action<Request>? onRequest = null)
        {
            this.responseJson = responseJson;
            this.onRequest = onRequest;
        }

        protected override async Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken)
        {
            var body = await request.Content!.ReadAsStringAsync(cancellationToken);
            onRequest?.Invoke(new Request(request.RequestUri!.ToString(), request.Headers.Authorization?.ToString(), JsonDocument.Parse(body).RootElement.Clone()));
            return new HttpResponseMessage(System.Net.HttpStatusCode.OK)
            {
                Content = new StringContent(responseJson, System.Text.Encoding.UTF8, "application/json")
            };
        }

        public sealed record Request(string Uri, string? Authorization, JsonElement Body);
    }
}
