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

    [Fact]
    public async Task HeartbeatAsync_posts_session_id_client_version_and_active_flag()
    {
        var handler = new BodyCapturingHandler(("""{"ok":true,"force_exit":false}""", HttpStatusCode.OK));
        var client = new OtaApiClient(new HttpClient(handler), baseUrl: "https://localhost:7243");

        var result = await client.HeartbeatAsync("sess-abc", active: true, CancellationToken.None);

        handler.Path.Should().Be("/api/heartbeat");
        handler.Body.Should().Contain("\"sessionId\":\"sess-abc\"")
            .And.Contain("\"active\":true")
            .And.Contain("\"clientVersion\"");
        result.ForceExit.Should().BeFalse();
        result.Reason.Should().BeNull();
    }

    [Fact]
    public async Task HeartbeatAsync_parses_force_exit_reason()
    {
        var client = new OtaApiClient(
            new HttpClient(new BodyCapturingHandler(("""{"ok":true,"force_exit":true,"reason":"违规下线"}""", HttpStatusCode.OK))),
            baseUrl: "https://localhost:7243");

        var result = await client.HeartbeatAsync("sess-abc", active: true, CancellationToken.None);

        result.ForceExit.Should().BeTrue();
        result.Reason.Should().Be("违规下线");
    }

    [Fact]
    public async Task HeartbeatAsync_maps_426_to_update_required()
    {
        var client = new OtaApiClient(
            new HttpClient(new BodyCapturingHandler((
                """{"error":"请更新 VivoKsu 到最新版本后继续使用。","code":"UPDATE_REQUIRED","latest":"2.0.0","min":"1.0.0","download_url":"https://x/VivoKsu-2.0.0.zip"}""",
                HttpStatusCode.UpgradeRequired))),
            baseUrl: "https://localhost:7243");

        var act = () => client.HeartbeatAsync("sess-abc", active: true, CancellationToken.None);

        var exception = await act.Should().ThrowAsync<UpdateRequiredException>();
        exception.Which.Latest.Should().Be("2.0.0");
        exception.Which.DownloadUrl.Should().Be("https://x/VivoKsu-2.0.0.zip");
    }

    [Fact]
    public async Task GetOnlineAsync_deserializes_sessions_and_self_flag()
    {
        var client = new OtaApiClient(
            new HttpClient(new StubHandler((
                """{"count":2,"sessions":[{"name":"张三","client_version":"1.0.0","connected_at":1000,"last_seen_at":1000,"duration_seconds":3600,"is_self":true},{"name":"李四","client_version":"1.0.0","connected_at":2000,"last_seen_at":2000,"duration_seconds":1800,"is_self":false}]}""",
                HttpStatusCode.OK))),
            baseUrl: "https://localhost:7243");

        var sessions = await client.GetOnlineAsync(CancellationToken.None);

        sessions.Should().HaveCount(2);
        sessions[0].Name.Should().Be("张三");
        sessions[0].DurationSeconds.Should().Be(3600);
        sessions[0].IsSelf.Should().BeTrue();
        sessions[1].Name.Should().Be("李四");
        sessions[1].IsSelf.Should().BeFalse();
    }

    [Fact]
    public async Task HeartbeatAsync_maps_401_to_ota_exception_even_with_non_json_body()
    {
        // 回归:状态码检查必须先于 body 解析——空/非 JSON 响应体(如 WAF HTML 403)不得抛
        // JsonException(会被心跳循环当网络抖动静默吞掉),而应抛 OtaApiException 触发强制退出。
        var handler = new RawHandler("<html><body>Forbidden</body></html>", HttpStatusCode.Forbidden);
        var client = new OtaApiClient(new HttpClient(handler), baseUrl: "https://localhost:7243");

        var act = () => client.HeartbeatAsync("sess-abc", active: true, CancellationToken.None);

        var exception = await act.Should().ThrowAsync<OtaApiException>();
        exception.Which.StatusCode.Should().Be(403);
    }

    [Fact]
    public async Task GetOnlineAsync_maps_401_to_ota_exception()
    {
        var client = new OtaApiClient(
            new HttpClient(new StubHandler(("""{"error":"API token 无效或已停用。"}""", HttpStatusCode.Unauthorized))),
            baseUrl: "https://localhost:7243");

        var act = () => client.GetOnlineAsync(CancellationToken.None);

        var exception = await act.Should().ThrowAsync<OtaApiException>();
        exception.Which.StatusCode.Should().Be(401);
    }

    private sealed class BodyCapturingHandler : HttpMessageHandler
    {
        private readonly string json;
        private readonly HttpStatusCode status;

        public BodyCapturingHandler((string Json, HttpStatusCode Status) response)
        {
            json = response.Json;
            status = response.Status;
        }

        public string? Body { get; private set; }

        public string? Path { get; private set; }

        protected override async Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken)
        {
            Path = request.RequestUri!.AbsolutePath;
            Body = request.Content is null ? string.Empty : await request.Content.ReadAsStringAsync(cancellationToken);
            return new HttpResponseMessage(status)
            {
                Content = new StringContent(json, System.Text.Encoding.UTF8, "application/json")
            };
        }
    }

    /// <summary>返回任意原始响应体(含非 JSON,如 WAF HTML)与指定状态码。</summary>
    private sealed class RawHandler : HttpMessageHandler
    {
        private readonly string body;
        private readonly HttpStatusCode status;

        public RawHandler(string body, HttpStatusCode status)
        {
            this.body = body;
            this.status = status;
        }

        protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken)
        {
            return Task.FromResult(new HttpResponseMessage(status)
            {
                Content = new StringContent(body, System.Text.Encoding.UTF8, "text/html")
            });
        }
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
