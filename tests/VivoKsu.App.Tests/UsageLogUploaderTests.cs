using System.Net;
using System.Net.Http;
using System.Text;
using FluentAssertions;
using VivoKsu.App.Models;
using VivoKsu.App.Services;

namespace VivoKsu.App.Tests;

public class UsageLogUploaderTests
{
    [Fact]
    public async Task Record_then_Flush_uploads_the_batch_and_clears_buffer()
    {
        var handler = new RecordingHandler();
        var client = new OtaApiClient(new HttpClient(handler), baseUrl: "https://localhost:7243");
        using var uploader = new UsageLogUploader(client);

        uploader.Record(new UsageLogEntry("Flashing", "正在刷写 boot", "success", "evt-1", 1000, 1060, 60000));
        uploader.Record(new UsageLogEntry("Rebooting", "正在重启设备", "canceled", "evt-2", 2000, 2010, 10000));

        await uploader.FlushAsync();

        handler.Requests.Should().HaveCount(1);
        handler.Requests[0].Path.Should().Be("/api/usage/logs");
        handler.Requests[0].Body.Should().Contain("\"operation\":\"Flashing\"")
            .And.Contain("\"operation\":\"Rebooting\"")
            .And.Contain("\"status\":\"canceled\"")
            // 字段名须 snake_case,与服务端契约一致(否则 started_at/ended_at/duration_ms 全丢)。
            .And.Contain("\"started_at\":1000")
            .And.Contain("\"ended_at\":1060")
            .And.Contain("\"duration_ms\":60000");
        uploader.PendingCount.Should().Be(0);
    }

    [Fact]
    public async Task Flush_with_empty_buffer_does_not_call_the_server()
    {
        var handler = new RecordingHandler();
        var client = new OtaApiClient(new HttpClient(handler), baseUrl: "https://localhost:7243");
        using var uploader = new UsageLogUploader(client);

        await uploader.FlushAsync();

        handler.Requests.Should().BeEmpty();
    }

    [Fact]
    public async Task Record_beyond_threshold_uploads_immediately()
    {
        var handler = new RecordingHandler();
        var client = new OtaApiClient(new HttpClient(handler), baseUrl: "https://localhost:7243");
        using var uploader = new UsageLogUploader(client);

        for (var i = 0; i < UsageLogUploader.FlushThreshold; i++)
        {
            uploader.Record(new UsageLogEntry("Flashing", $"op {i}", "success", "evt-" + i, i, i, 100));
        }

        await handler.WaitForRequestAsync(TimeSpan.FromSeconds(5));
        handler.Requests.Should().HaveCount(1);
        uploader.PendingCount.Should().Be(0);
    }

    [Fact]
    public async Task Upload_failure_is_swallowed_and_keeps_running()
    {
        var handler = new RecordingHandler(failNext: true);
        var client = new OtaApiClient(new HttpClient(handler), baseUrl: "https://localhost:7243");
        using var uploader = new UsageLogUploader(client);

        uploader.Record(new UsageLogEntry("Flashing", "正在刷写 boot", "success", "evt-1", 1000, 1060, 60000));

        var act = () => uploader.FlushAsync();

        // 失败不抛出(best-effort 上传),且批次放回缓冲供下次重试。
        await act.Should().NotThrowAsync();
        uploader.PendingCount.Should().Be(1);
    }

    [Fact]
    public async Task Flush_while_an_upload_is_in_flight_waits_then_uploads_the_tail()
    {
        // 回归:退出时若在途上传占用,FlushAsync 不能直接返回丢尾批——应等其完成后再传剩余。
        var handler = new RecordingHandler(delayMs: 100);
        var client = new OtaApiClient(new HttpClient(handler), baseUrl: "https://localhost:7243");
        using var uploader = new UsageLogUploader(client);

        uploader.Record(new UsageLogEntry("Flashing", "first", "success", "evt-first", 1, 2, 100));
        var firstFlush = uploader.FlushAsync(); // 在途(慢响应)
        uploader.Record(new UsageLogEntry("Rebooting", "tail", "success", "evt-tail", 3, 4, 100));

        await firstFlush;
        await uploader.FlushAsync(); // 退出路径的最终 flush:不得因在途而丢尾批

        handler.Requests.Should().HaveCount(2);
        handler.Requests.Select(r => r.Body).Should().Contain(b => b.Contains("first"));
        handler.Requests.Select(r => r.Body).Should().Contain(b => b.Contains("tail"));
        uploader.PendingCount.Should().Be(0);
    }

    private sealed class RecordingHandler : HttpMessageHandler
    {
        private readonly bool failNext;
        private readonly int delayMs;

        public RecordingHandler(bool failNext = false, int delayMs = 0)
        {
            this.failNext = failNext;
            this.delayMs = delayMs;
        }

        public List<(string Path, string Body)> Requests { get; } = [];

        protected override async Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken)
        {
            if (delayMs > 0)
            {
                await Task.Delay(delayMs, cancellationToken);
            }

            if (failNext)
            {
                throw new HttpRequestException("network down");
            }

            var body = request.Content is null ? string.Empty : await request.Content.ReadAsStringAsync(cancellationToken);
            Requests.Add((request.RequestUri!.AbsolutePath, body));
            return new HttpResponseMessage(HttpStatusCode.OK)
            {
                Content = new StringContent("""{"ok":true,"received":1}""", Encoding.UTF8, "application/json")
            };
        }

        public async Task WaitForRequestAsync(TimeSpan timeout)
        {
            var deadline = DateTime.UtcNow + timeout;
            while (DateTime.UtcNow < deadline)
            {
                if (Requests.Count > 0)
                {
                    return;
                }

                await Task.Delay(10);
            }

            throw new TimeoutException("等待上传超时");
        }
    }
}
