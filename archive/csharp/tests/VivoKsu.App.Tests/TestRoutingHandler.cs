using System.Net;
using System.Net.Http;

namespace VivoKsu.App.Tests;

/// <summary>
/// 按主机名路由的 <see cref="HttpMessageHandler"/>,模拟 GitHub 直连与各镜像的成败。
/// 记录所有请求以便断言 failover 顺序。
/// </summary>
public sealed class TestRoutingHandler : HttpMessageHandler
{
    private readonly Dictionary<string, Func<HttpResponseMessage>> routes = new(StringComparer.OrdinalIgnoreCase);
    private readonly List<Uri> requests = [];

    public IReadOnlyList<Uri> Requests => requests;

    public TestRoutingHandler Route(string host, HttpStatusCode status, byte[] content)
    {
        routes[host] = () =>
        {
            var response = new HttpResponseMessage(status)
            {
                Content = new ByteArrayContent(content)
            };
            return response;
        };
        return this;
    }

    protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken)
    {
        requests.Add(request.RequestUri!);
        if (routes.TryGetValue(request.RequestUri!.Host, out var factory))
        {
            return Task.FromResult(factory());
        }

        return Task.FromResult(new HttpResponseMessage(HttpStatusCode.NotFound));
    }
}
