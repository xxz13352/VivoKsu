using System.Net;
using System.Net.Http;
using FluentAssertions;
using VivoKsu.App.Models;
using VivoKsu.App.Services;

namespace VivoKsu.App.Tests;

public class OtaApiClientTests
{
    [Fact]
    public async Task ResolveAsync_queries_the_server_with_pd_and_version_and_deserializes_the_rom()
    {
        Uri? captured = null;
        var client = new OtaApiClient(
            new HttpClient(new StubHandler(
                ("""{"pd":"PD2057","version":"16.2.10.0.W10.V000L1","url":"https://sysuptxdl.vivo.com.cn/full.zip","name":"full","sizeBytes":1024}""",
                 HttpStatusCode.OK),
                uri => captured = uri)),
            baseUrl: "https://localhost:7243");

        var rom = await client.ResolveAsync("PD2057", "16.2.10.0.W10.V000L1", CancellationToken.None);

        captured.Should().Be("https://localhost:7243/api/rom?pd=PD2057&version=16.2.10.0.W10.V000L1");
        rom.Url.Should().Be("https://sysuptxdl.vivo.com.cn/full.zip");
        rom.Pd.Should().Be("PD2057");
        rom.SizeBytes.Should().Be(1024);
    }

    [Fact]
    public async Task ResolveAsync_escapes_special_characters_in_query_parameters()
    {
        Uri? captured = null;
        var client = new OtaApiClient(
            new HttpClient(new StubHandler(("""{"pd":"P","version":"v","url":"https://x/y"}""", HttpStatusCode.OK), uri => captured = uri)),
            baseUrl: "https://localhost:7243");

        await client.ResolveAsync("PD 2057", "16.2.10.0/W30", CancellationToken.None);

        captured!.Query.Should().Contain("pd=PD%202057").And.Contain("version=16.2.10.0%2FW30");
    }

    [Fact]
    public async Task ResolveAsync_maps_not_found_to_a_chinese_message()
    {
        var client = new OtaApiClient(
            new HttpClient(new StubHandler(("""{"error":"未找到 PD2057 对应的 ROM。"}""", HttpStatusCode.NotFound))),
            baseUrl: "https://localhost:7243");

        var act = () => client.ResolveAsync("PD2057", "nope", CancellationToken.None);

        var exception = await act.Should().ThrowAsync<OtaApiException>();
        exception.Which.StatusCode.Should().Be(404);
        exception.Which.Message.Should().Contain("未找到");
    }

    [Fact]
    public async Task ResolveAsync_maps_insufficient_credits_status()
    {
        var client = new OtaApiClient(
            new HttpClient(new StubHandler(("""{"error":"VOTA 未能解析 ROM 下载链接。(INSUFFICIENT_CREDITS)"}""", HttpStatusCode.PaymentRequired))),
            baseUrl: "https://localhost:7243");

        var act = () => client.ResolveAsync("PD2057", "v", CancellationToken.None);

        var exception = await act.Should().ThrowAsync<OtaApiException>();
        exception.Which.StatusCode.Should().Be(402);
        exception.Which.Message.Should().Contain("INSUFFICIENT_CREDITS");
    }

    private sealed class StubHandler : HttpMessageHandler
    {
        private readonly string json;
        private readonly HttpStatusCode status;
        private readonly Action<Uri>? onRequest;

        public StubHandler((string Json, HttpStatusCode Status) response, Action<Uri>? onRequest = null)
        {
            json = response.Json;
            status = response.Status;
            this.onRequest = onRequest;
        }

        protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken)
        {
            onRequest?.Invoke(request.RequestUri!);
            return Task.FromResult(new HttpResponseMessage(status)
            {
                Content = new StringContent(json, System.Text.Encoding.UTF8, "application/json")
            });
        }
    }
}
