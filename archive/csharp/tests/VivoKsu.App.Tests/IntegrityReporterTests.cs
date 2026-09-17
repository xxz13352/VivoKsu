using System.Net;
using System.Net.Http;
using System.Text;
using FluentAssertions;
using VivoKsu.App.Models;
using VivoKsu.App.Services;

namespace VivoKsu.App.Tests;

public class IntegrityReporterTests
{
    [Fact]
    public async Task Report_posts_a_valid_integrity_event()
    {
        var handler = new RecordingHandler();
        var client = new OtaApiClient(new HttpClient(handler), baseUrl: "https://localhost:7243");
        var reporter = new IntegrityReporter(client);

        reporter.Report(IntegrityReportPhase.Heartbeat, IntegrityReportReason.SequenceRollback);
        await handler.WaitForRequestAsync(TimeSpan.FromSeconds(5));

        handler.Requests.Should().HaveCount(1);
        var (path, body) = handler.Requests[0];
        path.Should().Be("/api/integrity/report");
        body.Should().Contain("\"phase\":\"heartbeat\"")
            .And.Contain("\"reason\":\"sequence_rollback\"")
            .And.Contain("\"client_version\"")
            .And.Contain("\"build_id\"")
            .And.Contain("\"occurred_at\":");
    }

    [Fact]
    public async Task Report_is_rate_limited_within_the_minimum_gap()
    {
        // 防抖:同 10s 窗口内的第二次上报被客户端挡掉,不产生网络往返(服务端窗口 60s/20 条)。
        var handler = new RecordingHandler();
        var client = new OtaApiClient(new HttpClient(handler), baseUrl: "https://localhost:7243");
        var reporter = new IntegrityReporter(client);

        reporter.Report(IntegrityReportPhase.Startup, IntegrityReportReason.PinMismatch);
        await handler.WaitForRequestAsync(TimeSpan.FromSeconds(5));

        reporter.Report(IntegrityReportPhase.Heartbeat, IntegrityReportReason.LeaseExpired);
        await Task.Delay(100);

        handler.Requests.Should().HaveCount(1);
    }

    [Fact]
    public async Task Report_failure_is_silent_and_does_not_throw()
    {
        var handler = new RecordingHandler(fail: true);
        var client = new OtaApiClient(new HttpClient(handler), baseUrl: "https://localhost:7243");
        var reporter = new IntegrityReporter(client);

        var act = () => reporter.Report(IntegrityReportPhase.Startup, IntegrityReportReason.PinMismatch);

        act.Should().NotThrow(); // fire-and-forget:异常不逃逸到调用线程
        await handler.WaitForRequestAsync(TimeSpan.FromSeconds(5)); // 确实尝试了上报
    }

    private sealed class RecordingHandler : HttpMessageHandler
    {
        private readonly bool fail;
        private readonly TaskCompletionSource<bool> received =
            new(TaskCreationOptions.RunContinuationsAsynchronously);

        public RecordingHandler(bool fail = false) => this.fail = fail;

        public List<(string Path, string Body)> Requests { get; } = [];

        protected override async Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken)
        {
            var body = request.Content is null
                ? string.Empty
                : await request.Content.ReadAsStringAsync(cancellationToken);
            Requests.Add((request.RequestUri!.AbsolutePath, body));
            received.TrySetResult(true);
            if (fail)
            {
                throw new HttpRequestException("network down");
            }

            return new HttpResponseMessage(HttpStatusCode.OK)
            {
                Content = new StringContent("""{"ok":true}""", Encoding.UTF8, "application/json"),
            };
        }

        public async Task WaitForRequestAsync(TimeSpan timeout)
        {
            await received.Task.WaitAsync(timeout);
        }
    }
}
