using System.Net;
using System.Net.Http;
using System.Text;
using FluentAssertions;
using VivoKsu.App.Services;

namespace VivoKsu.App.Tests;

public class HeartbeatServiceTests
{
    private static readonly TimeSpan Tick = TimeSpan.FromMilliseconds(20);

    [Fact]
    public async Task Force_exit_response_invokes_callback_with_reason_and_stops_beating()
    {
        var handler = new RecordingHandler(_ => Task.FromResult(
            (HttpStatusCode.OK, """{"ok":true,"force_exit":true,"reason":"违规下线"}""")));
        var client = new OtaApiClient(new HttpClient(handler), baseUrl: "https://localhost:7243");
        var forced = new TaskCompletionSource<string>(TaskCreationOptions.RunContinuationsAsynchronously);
        using var service = new HeartbeatService(
            client,
            onForceExitAsync: reason => { forced.TrySetResult(reason); return Task.CompletedTask; },
            heartbeatInterval: Tick);

        service.Start("sess-1");

        var reason = await forced.Task.WaitAsync(TimeSpan.FromSeconds(5));
        reason.Should().Be("违规下线");

        // force_exit 后循环停止:不再发新心跳。
        var countAfterExit = handler.Count;
        await Task.Delay(80);
        handler.Count.Should().Be(countAfterExit);
    }

    [Fact]
    public async Task StopAsync_sends_goodbye_with_active_false_after_cancelling_the_loop()
    {
        var handler = new RecordingHandler(_ => Task.FromResult(
            (HttpStatusCode.OK, """{"ok":true,"force_exit":false}""")));
        var client = new OtaApiClient(new HttpClient(handler), baseUrl: "https://localhost:7243");
        using var service = new HeartbeatService(client, heartbeatInterval: Tick);

        service.Start("sess-1");
        await handler.WaitForHeartbeatsAsync(1, TimeSpan.FromSeconds(5));

        await service.StopAsync();

        var heartbeats = handler.Requests.Where(r => r.Path == "/api/heartbeat").ToList();
        heartbeats.Should().Contain(r => r.Body.Contains("\"active\":true"));
        heartbeats.Should().Contain(r => r.Body.Contains("\"active\":false"));
        heartbeats.Should().Contain(r => r.Body.Contains("sess-1"));
    }

    [Fact]
    public async Task Transient_failure_keeps_beating_and_recovers()
    {
        var handler = new RecordingHandler(_ => Task.FromResult(
            (HttpStatusCode.OK, """{"ok":true,"force_exit":false}""")));
        handler.FailNext = true;
        var client = new OtaApiClient(new HttpClient(handler), baseUrl: "https://localhost:7243");
        using var service = new HeartbeatService(client, heartbeatInterval: Tick);

        service.Start("sess-1");

        // 第一次失败(网络抖动)后仍继续心跳并恢复健康。
        await handler.WaitForHeartbeatsAsync(2, TimeSpan.FromSeconds(5));
        service.IsHealthy.Should().BeTrue();

        await service.StopAsync();
    }

    [Fact]
    public async Task StopAsync_called_from_force_exit_callback_does_not_deadlock()
    {
        // 回归:回调由心跳循环内调用,回调里 Stop() 若等待循环任务会「自己等自己」死锁。
        var handler = new RecordingHandler(_ => Task.FromResult(
            (HttpStatusCode.OK, """{"ok":true,"force_exit":true,"reason":"kick"}""")));
        var client = new OtaApiClient(new HttpClient(handler), baseUrl: "https://localhost:7243");
        var callbackDone = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);
        HeartbeatService? service = null;
        using (service = new HeartbeatService(
                   client,
                   onForceExitAsync: async reason =>
                   {
                       await service!.StopAsync(); // 回调内 Stop:不得死锁
                       callbackDone.TrySetResult(true);
                   },
                   heartbeatInterval: Tick))
        {
            service.Start("sess-1");

            (await callbackDone.Task.WaitAsync(TimeSpan.FromSeconds(5))).Should().BeTrue();
            handler.Requests.Should().Contain(r => r.Body.Contains("\"active\":false")); // goodbye 已发
        }
    }

    [Fact]
    public async Task IsRunning_becomes_false_after_self_termination()
    {
        // 回归:循环因 force_exit 自终止后,IsRunning 应反映真实状态(否则在线页一直显示「心跳:正常」)。
        var handler = new RecordingHandler(_ => Task.FromResult(
            (HttpStatusCode.OK, """{"ok":true,"force_exit":true,"reason":"kick"}""")));
        var client = new OtaApiClient(new HttpClient(handler), baseUrl: "https://localhost:7243");
        var callbackDone = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);
        using var service = new HeartbeatService(
            client,
            onForceExitAsync: reason => { callbackDone.TrySetResult(true); return Task.CompletedTask; },
            heartbeatInterval: Tick);

        service.Start("sess-1");
        await callbackDone.Task.WaitAsync(TimeSpan.FromSeconds(5));

        // 回调返回后循环自行结束,IsRunning 变 false。
        var deadline = DateTime.UtcNow + TimeSpan.FromSeconds(5);
        while (service.IsRunning && DateTime.UtcNow < deadline)
        {
            await Task.Delay(10);
        }

        service.IsRunning.Should().BeFalse();
    }

    [Fact]
    public async Task Update_required_stops_the_loop_and_invokes_callback()
    {
        var handler = new RecordingHandler(_ => Task.FromResult(
            (HttpStatusCode.UpgradeRequired,
                """{"error":"请更新 VivoKsu 到最新版本后继续使用。","code":"UPDATE_REQUIRED","latest":"2.0.0","min":"1.0.0","download_url":"https://x/VivoKsu-2.0.0.zip"}""")));
        var client = new OtaApiClient(new HttpClient(handler), baseUrl: "https://localhost:7243");
        var updated = new TaskCompletionSource<string>(TaskCreationOptions.RunContinuationsAsynchronously);
        using var service = new HeartbeatService(
            client,
            onUpdateRequiredAsync: update => { updated.TrySetResult(update.Latest ?? string.Empty); return Task.CompletedTask; },
            heartbeatInterval: Tick);

        service.Start("sess-1");

        (await updated.Task.WaitAsync(TimeSpan.FromSeconds(5))).Should().Be("2.0.0");
        handler.Count.Should().Be(1); // 426 后不再重试
    }

    /// <summary>记录每次请求;响应函数返回 (status, json);FailNext 让下一次请求抛网络异常。</summary>
    private sealed class RecordingHandler : HttpMessageHandler
    {
        private readonly Func<HttpRequestMessage, Task<(HttpStatusCode Status, string Json)>> respond;

        public RecordingHandler(Func<HttpRequestMessage, Task<(HttpStatusCode Status, string Json)>> respond)
        {
            this.respond = respond;
        }

        public List<(string Path, string Body)> Requests { get; } = [];

        public int Count => Requests.Count;

        public bool FailNext { get; set; }

        protected override async Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken)
        {
            var body = request.Content is null ? string.Empty : await request.Content.ReadAsStringAsync(cancellationToken);
            Requests.Add((request.RequestUri!.AbsolutePath, body));
            if (FailNext)
            {
                FailNext = false;
                throw new HttpRequestException("network down");
            }

            var (status, json) = await respond(request);
            return new HttpResponseMessage(status)
            {
                Content = new StringContent(json, Encoding.UTF8, "application/json")
            };
        }

        public async Task WaitForHeartbeatsAsync(int count, TimeSpan timeout)
        {
            var deadline = DateTime.UtcNow + timeout;
            while (DateTime.UtcNow < deadline)
            {
                if (Requests.Count(r => r.Path == "/api/heartbeat") >= count)
                {
                    return;
                }

                await Task.Delay(10);
            }

            throw new TimeoutException($"等待 {count} 个心跳超时,实际 {Requests.Count(r => r.Path == "/api/heartbeat")} 个");
        }
    }
}
