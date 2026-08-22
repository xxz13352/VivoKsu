using System.Net;
using System.Net.Http;
using System.Text;
using FluentAssertions;
using VivoKsu.App.Services;
using VivoKsu.App.ViewModels;

namespace VivoKsu.App.Tests;

public class OnlineViewModelTests
{
    [Fact]
    public async Task Refresh_populates_sessions_with_self_marker_and_duration()
    {
        var handler = new StubHandler((
            """{"count":2,"sessions":[{"name":"张三","client_version":"1.0.0","connected_at":100000,"last_seen_at":100000,"duration_seconds":3600,"is_self":true},{"name":"李四","client_version":"1.2.0","connected_at":200000,"last_seen_at":200000,"duration_seconds":1800,"is_self":false}]}""",
            HttpStatusCode.OK));
        var client = new OtaApiClient(new HttpClient(handler), baseUrl: "https://localhost:7243");
        var viewModel = new OnlineViewModel(client, new HeartbeatService(client));

        await viewModel.RefreshCommand.ExecuteAsync(null);

        viewModel.Sessions.Should().HaveCount(2);
        viewModel.OnlineCount.Should().Be(2);
        viewModel.Sessions[0].Name.Should().Be("张三");
        viewModel.Sessions[0].IsSelf.Should().BeTrue();
        viewModel.Sessions[0].DurationText.Should().NotBeNullOrWhiteSpace();
        viewModel.Sessions[1].IsSelf.Should().BeFalse();
        viewModel.Sessions[1].ClientVersion.Should().Be("1.2.0");
        viewModel.StatusText.Should().Contain("2");
        viewModel.LastUpdatedText.Should().StartWith("更新于");
    }

    [Fact]
    public async Task Refresh_with_empty_list_sets_idle_status()
    {
        var client = new OtaApiClient(
            new HttpClient(new StubHandler(("""{"count":0,"sessions":[]}""", HttpStatusCode.OK))),
            baseUrl: "https://localhost:7243");
        var viewModel = new OnlineViewModel(client, new HeartbeatService(client));

        await viewModel.RefreshCommand.ExecuteAsync(null);

        viewModel.OnlineCount.Should().Be(0);
        viewModel.StatusText.Should().Contain("没有在线用户");
    }

    [Fact]
    public async Task Refresh_with_network_failure_reports_stale_status()
    {
        var client = new OtaApiClient(
            new HttpClient(new StubHandler(("""{"error":"boom"}""", HttpStatusCode.InternalServerError))),
            baseUrl: "https://localhost:7243");
        var viewModel = new OnlineViewModel(client, new HeartbeatService(client));

        await viewModel.RefreshCommand.ExecuteAsync(null);

        viewModel.StatusText.Should().Contain("无法连接服务器");
    }

    [Fact]
    public void OnlineSessionItem_duration_text_counts_up_from_connected_time()
    {
        var now = DateTimeOffset.UtcNow.ToUnixTimeSeconds();
        var item = new OnlineSessionItem(
            new Models.OnlineSession("张三", "1.0.0", now - 3661, now, 3661, IsSelf: true),
            now);

        item.DurationText.Should().Be("01:01:01");

        item.UpdateDuration(now + 60);
        item.DurationText.Should().Be("01:02:01");
    }

    [Fact]
    public void Stop_is_idempotent_and_dispose_does_not_throw()
    {
        var client = new OtaApiClient(
            new HttpClient(new StubHandler(("""{"count":0,"sessions":[]}""", HttpStatusCode.OK))),
            baseUrl: "https://localhost:7243");
        var viewModel = new OnlineViewModel(client, new HeartbeatService(client));

        viewModel.Stop();
        viewModel.Stop();
        viewModel.Dispose();
    }

    private sealed class StubHandler : HttpMessageHandler
    {
        private readonly string json;
        private readonly HttpStatusCode status;

        public StubHandler((string Json, HttpStatusCode Status) response)
        {
            json = response.Json;
            status = response.Status;
        }

        protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken)
        {
            return Task.FromResult(new HttpResponseMessage(status)
            {
                Content = new StringContent(json, Encoding.UTF8, "application/json")
            });
        }
    }
}
