using System.Net;
using System.Net.Http;
using System.Text;
using FluentAssertions;
using VivoKsu.App.Models;
using VivoKsu.App.Services;

namespace VivoKsu.App.Tests;

public class ServerOperationGateTests
{
    [Fact]
    public async Task AuthorizeAsync_allows_when_server_allows()
    {
        var gate = CreateGate("""{"allowed":true}""", HttpStatusCode.OK);

        var result = await gate.AuthorizeAsync(OperationKind.Flashing, "正在刷写 boot", CancellationToken.None);

        result.Allowed.Should().BeTrue();
    }

    [Fact]
    public async Task AuthorizeAsync_denies_when_server_rejects_with_reason()
    {
        var gate = CreateGate("""{"allowed":false,"reason":"账号已被封禁,请联系管理员。"}""", HttpStatusCode.OK);

        var result = await gate.AuthorizeAsync(OperationKind.Flashing, "正在刷写 boot", CancellationToken.None);

        result.Allowed.Should().BeFalse();
        result.Reason.Should().Contain("封禁");
    }

    [Fact]
    public async Task AuthorizeAsync_denies_when_token_invalid_401()
    {
        var gate = CreateGate("""{"error":"API token 无效或已停用。"}""", HttpStatusCode.Unauthorized);

        var result = await gate.AuthorizeAsync(OperationKind.Flashing, "正在刷写 boot", CancellationToken.None);

        result.Allowed.Should().BeFalse();
        result.Reason.Should().Contain("登录已失效");
    }

    [Fact]
    public async Task AuthorizeAsync_fails_open_on_network_error()
    {
        // 服务端默认许可;网络不可达不应阻塞刷写(账号封禁由心跳 5s 内强制退出兜底)。
        var handler = new ThrowingHandler(new HttpRequestException("network down"));
        var gate = new ServerOperationGate(new OtaApiClient(new HttpClient(handler), baseUrl: "https://localhost:7243"));

        var result = await gate.AuthorizeAsync(OperationKind.Flashing, "正在刷写 boot", CancellationToken.None);

        result.Allowed.Should().BeTrue();
    }

    [Fact]
    public async Task AuthorizeAsync_fails_open_on_server_500()
    {
        var gate = CreateGate("""{"error":"内部错误。"}""", HttpStatusCode.InternalServerError);

        var result = await gate.AuthorizeAsync(OperationKind.Rebooting, "正在重启设备", CancellationToken.None);

        result.Allowed.Should().BeTrue();
    }

    private static ServerOperationGate CreateGate(string json, HttpStatusCode status) =>
        new(new OtaApiClient(new HttpClient(new StubHandler((json, status))), baseUrl: "https://localhost:7243"));

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

    private sealed class ThrowingHandler : HttpMessageHandler
    {
        private readonly Exception exception;

        public ThrowingHandler(Exception exception)
        {
            this.exception = exception;
        }

        protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken)
            => Task.FromException<HttpResponseMessage>(exception);
    }
}
