using System.Net;
using System.Text;
using System.Text.Json;
using VivoKsu.App.Services;

namespace VivoKsu.App.Tests;

/// <summary>VivoKsu 版本门禁:启动版本校验与 426 强制更新解析。</summary>
public class AppVersionControlTests
{
    [Fact]
    public async Task CheckAsync_force_update_payload_parses_force_flag()
    {
        using var service = new AppVersionService(
            CreateClient("""{"latest":"1.2.0","min":"1.0.0","download_url":"https://dl/1.2.0.zip","update_required":true,"force_update":true}"""),
            "https://test");

        var result = await service.CheckAsync(CancellationToken.None);

        Assert.True(result.ForceUpdate);
        Assert.True(result.UpdateRequired);
        Assert.Equal("1.2.0", result.Latest);
        Assert.Equal("1.0.0", result.MinVersion);
        Assert.Equal("https://dl/1.2.0.zip", result.DownloadUrl);
    }

    [Fact]
    public async Task CheckAsync_no_policy_allows_all()
    {
        using var service = new AppVersionService(
            CreateClient("""{"latest":null,"min":null,"download_url":null,"update_required":false,"force_update":false}"""),
            "https://test");

        var result = await service.CheckAsync(CancellationToken.None);

        Assert.False(result.ForceUpdate);
        Assert.False(result.UpdateRequired);
        Assert.Null(result.Latest);
    }

    [Fact]
    public async Task CheckAsync_network_failure_returns_allow_all()
    {
        using var service = new AppVersionService(
            CreateClient(_ => throw new HttpRequestException("offline")),
            "https://test");

        var result = await service.CheckAsync(CancellationToken.None);

        Assert.False(result.ForceUpdate);
        Assert.False(result.UpdateRequired);
    }

    [Fact]
    public void UpdateRequiredException_from_response_parses_fields()
    {
        using var doc = JsonDocument.Parse(
            """{"error":"请更新 VivoKsu。","code":"UPDATE_REQUIRED","latest":"1.2.0","min":"1.0.0","download_url":"https://dl"}""");

        var ex = UpdateRequiredException.FromResponse(doc.RootElement);

        Assert.Equal("请更新 VivoKsu。", ex.Message);
        Assert.Equal("1.2.0", ex.Latest);
        Assert.Equal("1.0.0", ex.MinVersion);
        Assert.Equal("https://dl", ex.DownloadUrl);
    }

    [Fact]
    public void UpdateRequiredException_from_response_missing_fields_falls_back()
    {
        using var doc = JsonDocument.Parse("""{"code":"UPDATE_REQUIRED"}""");

        var ex = UpdateRequiredException.FromResponse(doc.RootElement);

        Assert.Equal("需要更新 VivoKsu 后才能继续使用。", ex.Message);
        Assert.Null(ex.Latest);
        Assert.Null(ex.MinVersion);
        Assert.Null(ex.DownloadUrl);
    }

    private static HttpClient CreateClient(string json) =>
        new(new FakeHandler(_ => new HttpResponseMessage(HttpStatusCode.OK)
        {
            Content = new StringContent(json, Encoding.UTF8, "application/json"),
        }));

    private static HttpClient CreateClient(Func<HttpRequestMessage, HttpResponseMessage> respond) =>
        new(new FakeHandler(respond));

    private sealed class FakeHandler : HttpMessageHandler
    {
        private readonly Func<HttpRequestMessage, HttpResponseMessage> respond;

        public FakeHandler(Func<HttpRequestMessage, HttpResponseMessage> respond) => this.respond = respond;

        protected override Task<HttpResponseMessage> SendAsync(
            HttpRequestMessage request, CancellationToken cancellationToken)
            => Task.FromResult(respond(request));
    }
}
