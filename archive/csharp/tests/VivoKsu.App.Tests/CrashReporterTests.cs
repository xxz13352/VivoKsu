using System.Net;
using System.Net.Http;
using System.Text;
using FluentAssertions;
using VivoKsu.App.Models;
using VivoKsu.App.Services;

namespace VivoKsu.App.Tests;

public class CrashReporterTests : IDisposable
{
    private readonly string crashLogPath = Path.Combine(
        Path.GetTempPath(), "VivoKsu.Tests", $"crash-{Guid.NewGuid():N}", "crash.log");

    public void Dispose()
    {
        try
        {
            var directory = Path.GetDirectoryName(crashLogPath);
            if (directory is not null && Directory.Exists(directory))
            {
                Directory.Delete(directory, recursive: true);
            }
        }
        catch
        {
            // 临时目录清不掉不影响测试结论。
        }
    }

    [Fact]
    public void Parse_extracts_only_panic_lines_with_epoch_prefix()
    {
        // 对齐 Rust parse_crash_log:只认 [epoch] panic: ...;旧时间戳格式与损坏行忽略。
        var log = "[1787444800] panic: panicked at src/main.rs:42:5:\n" +
                  "[2026-09-05 22:00:00] System.NullReferenceException: old format\n" +
                  "[bad] panic: malformed timestamp\n" +
                  "[1787445000] panic: second panic";

        var entries = CrashReporter.Parse(log);

        entries.Should().HaveCount(2);
        entries[0].OccurredAtEpochSeconds.Should().Be(1787444800);
        entries[0].PanicText.Should().Be("panicked at src/main.rs:42:5:");
        entries[1].OccurredAtEpochSeconds.Should().Be(1787445000);
        entries[1].PanicText.Should().Be("second panic");
    }

    [Fact]
    public void BuildReport_uses_newest_panic_as_message_and_older_as_backtrace()
    {
        var reporter = new CrashReporter(crashLogPath);
        var entries = new[]
        {
            new PendingCrashEntry(1000, "old panic"),
            new PendingCrashEntry(2000, "newest panic"),
        };

        var report = reporter.BuildReport(entries, "0123456789abcdef");

        report.Should().NotBeNull();
        report!.PanicMessage.Should().Be("newest panic");
        report.Backtrace.Should().Be("old panic\n");
        report.OccurredAtEpochSeconds.Should().Be(2000);
        report.EventId.Should().StartWith("crash-2000-");
        report.ClientVersion.Should().Be(AppInfo.Version);
        report.BuildId.Should().Be(ClientSession.BuildId);
    }

    [Fact]
    public void BuildReport_sanitizes_event_id_and_drops_high_risk_entries()
    {
        var reporter = new CrashReporter(crashLogPath);
        var entries = new[]
        {
            new PendingCrashEntry(42, "boom with spaces!!"), // event_id 需清洗
            new PendingCrashEntry(43, "leak -----BEGIN RSA PRIVATE KEY-----"), // 高危:整条丢弃
        };

        var report = reporter.BuildReport(entries, "0123456789abcdef");

        report.Should().NotBeNull();
        report!.EventId.Should().MatchRegex("^[A-Za-z0-9._:-]+$");
        report.PanicMessage.Should().NotContain("PRIVATE KEY"); // 最新条目被丢后回退到上一条
        report.PanicMessage.Should().Be("boom with spaces!!");
        report.Backtrace.Should().BeEmpty();
    }

    [Fact]
    public void BuildReport_redacts_credential_like_content_but_keeps_diagnostics()
    {
        var reporter = new CrashReporter(crashLogPath);
        var entries = new[]
        {
            new PendingCrashEntry(1, "login failed token=abcdef123456 at C:\\logs\\app.txt"),
        };

        var report = reporter.BuildReport(entries, "0123456789abcdef");

        report!.PanicMessage.Should().Contain("token=[CREDENTIAL_REMOVED]")
            .And.Contain("C:\\logs\\app.txt"); // 路径有诊断价值,保留
        report.PanicMessage.Should().NotContain("abcdef123456");
    }

    [Fact]
    public void BuildReport_returns_null_without_usable_entries()
    {
        var reporter = new CrashReporter(crashLogPath);

        reporter.BuildReport([], "s").Should().BeNull();
        reporter.BuildReport([new PendingCrashEntry(1, "-----BEGIN")], "s").Should().BeNull();
    }

    [Fact]
    public async Task UploadPending_uploads_and_clears_the_log_on_success()
    {
        var handler = new StatusHandler(HttpStatusCode.Accepted);
        var client = new OtaApiClient(new HttpClient(handler), baseUrl: "https://localhost:7243");
        var reporter = new CrashReporter(crashLogPath);
        reporter.Write(new InvalidOperationException("boom at Startup"));

        (await reporter.UploadPendingAsync(client, "0123456789abcdef", CancellationToken.None)).Should().BeTrue();
        handler.Requests.Should().HaveCount(1);
        handler.Requests[0].Path.Should().Be("/api/diagnostics/crash");
        handler.Requests[0].Body.Should().Contain("\"panic_message\"").And.Contain("boom at Startup");
        File.Exists(crashLogPath).Should().BeTrue();
        File.ReadAllText(crashLogPath).Should().BeEmpty();
    }

    [Fact]
    public async Task UploadPending_keeps_the_log_when_the_server_rejects()
    {
        var handler = new StatusHandler(HttpStatusCode.TooManyRequests);
        var client = new OtaApiClient(new HttpClient(handler), baseUrl: "https://localhost:7243");
        var reporter = new CrashReporter(crashLogPath);
        reporter.Write(new InvalidOperationException("boom"));

        (await reporter.UploadPendingAsync(client, "0123456789abcdef", CancellationToken.None)).Should().BeFalse();
        File.ReadAllText(crashLogPath).Should().Contain("boom"); // 保留,下次启动重试
    }

    [Fact]
    public async Task UploadPending_does_nothing_when_the_log_is_absent()
    {
        var handler = new StatusHandler(HttpStatusCode.Accepted);
        var client = new OtaApiClient(new HttpClient(handler), baseUrl: "https://localhost:7243");
        var reporter = new CrashReporter(crashLogPath);

        (await reporter.UploadPendingAsync(client, "0123456789abcdef", CancellationToken.None)).Should().BeFalse();
        handler.Requests.Should().BeEmpty();
    }

    private sealed class StatusHandler : HttpMessageHandler
    {
        private readonly HttpStatusCode status;

        public StatusHandler(HttpStatusCode status) => this.status = status;

        public List<(string Path, string Body)> Requests { get; } = [];

        protected override async Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken)
        {
            var body = request.Content is null
                ? string.Empty
                : await request.Content.ReadAsStringAsync(cancellationToken);
            Requests.Add((request.RequestUri!.AbsolutePath, body));
            return new HttpResponseMessage(status)
            {
                Content = new StringContent("""{"ok":true}""", Encoding.UTF8, "application/json"),
            };
        }
    }
}
