using System.Net;
using System.Net.Http;
using System.Text;
using FluentAssertions;
using VivoKsu.App.Models;
using VivoKsu.App.Services;

namespace VivoKsu.App.Tests;

public class UsageLogUploaderTests : IDisposable
{
    private readonly string spoolDirectory = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", $"usage-{Guid.NewGuid():N}");

    public void Dispose()
    {
        try
        {
            if (Directory.Exists(spoolDirectory))
            {
                Directory.Delete(spoolDirectory, recursive: true);
            }
        }
        catch
        {
            // 临时目录清不掉不影响测试结论。
        }
    }

    [Fact]
    public async Task Record_then_Flush_uploads_the_batch_and_clears_buffer()
    {
        var handler = new RecordingHandler();
        var client = new OtaApiClient(new HttpClient(handler), baseUrl: "https://localhost:7243");
        using var uploader = new UsageLogUploader(client);
        uploader.PublishSession("tester", "gen-1");

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
        uploader.PublishSession("tester", "gen-1");

        await uploader.FlushAsync();

        handler.Requests.Should().BeEmpty();
    }

    [Fact]
    public async Task Record_beyond_threshold_uploads_immediately()
    {
        var handler = new RecordingHandler();
        var client = new OtaApiClient(new HttpClient(handler), baseUrl: "https://localhost:7243");
        using var uploader = new UsageLogUploader(client);
        uploader.PublishSession("tester", "gen-1");
        uploader.Start();

        for (var i = 0; i < UsageLogUploader.FlushThreshold; i++)
        {
            uploader.Record(new UsageLogEntry("Flashing", $"op {i}", "success", "evt-" + i, i, i, 100));
        }

        await handler.WaitForRequestAsync(TimeSpan.FromSeconds(5));
        handler.Requests.Should().HaveCount(1);
        uploader.PendingCount.Should().Be(0);
    }

    [Fact]
    public async Task Transient_upload_failure_is_swallowed_and_keeps_running()
    {
        var handler = new RecordingHandler(failNext: true);
        var client = new OtaApiClient(new HttpClient(handler), baseUrl: "https://localhost:7243");
        using var uploader = new UsageLogUploader(client);
        uploader.PublishSession("tester", "gen-1");

        uploader.Record(new UsageLogEntry("Flashing", "正在刷写 boot", "success", "evt-1", 1000, 1060, 60000));

        var act = () => uploader.FlushAsync();

        // 失败不抛出(best-effort 上传),且批次放回队列供下次重试。
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
        uploader.PublishSession("tester", "gen-1");

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

    // ---------------- 对齐 Rust 新增的能力 ----------------

    [Fact]
    public async Task Record_is_ignored_before_a_session_is_published()
    {
        // 对齐 Rust:未绑定登录会话(credential 为 None)时不记录 —— 不然不知道归属哪个账号。
        var handler = new RecordingHandler();
        var client = new OtaApiClient(new HttpClient(handler), baseUrl: "https://localhost:7243");
        using var uploader = new UsageLogUploader(client);

        uploader.Record(new UsageLogEntry("Flashing", "orphan", "success", "evt-orphan", 1, 2, 100));
        await uploader.FlushAsync();

        handler.Requests.Should().BeEmpty();
        uploader.PendingCount.Should().Be(0);
    }

    [Fact]
    public async Task Operation_details_are_uploaded_with_the_entry()
    {
        // 对齐 Rust UsageLogEntry::details:过程日志随记录上传,服务端存 details_json。
        var handler = new RecordingHandler();
        var client = new OtaApiClient(new HttpClient(handler), baseUrl: "https://localhost:7243");
        using var uploader = new UsageLogUploader(client);
        uploader.PublishSession("tester", "gen-1");

        uploader.Record(new UsageLogEntry(
            "Flashing", "正在刷写 boot", "failed", "evt-1", 1000, 1060, 60000,
            [
                new UsageLogDetail(1001, "Info", "解锁 boot 分区"),
                new UsageLogDetail(1020, "Warning", "写入较慢"),
                new UsageLogDetail(1055, "Error", "分区写入失败: EPIPE"),
            ]));

        await uploader.FlushAsync();

        var body = handler.Requests.Single().Body;
        // 默认序列化会把非 ASCII 转义成 \uXXXX,因此只断言结构与 ASCII 部分。
        body.Should().Contain("\"details\":[{\"timestamp_utc\":1001,\"level\":\"Info\"")
            .And.Contain("\"timestamp_utc\":1020,\"level\":\"Warning\"")
            .And.Contain("\"level\":\"Error\"")
            .And.Contain("\\u5206") // 中文按 \uXXXX 转义:「分」
            .And.Contain("EPIPE");
    }

    [Fact]
    public async Task Entries_without_details_do_not_serialize_an_empty_details_array()
    {
        // 对齐 Rust 的 skip_serializing_if = Vec::is_empty:空明细不发字段,不放大请求体。
        var handler = new RecordingHandler();
        var client = new OtaApiClient(new HttpClient(handler), baseUrl: "https://localhost:7243");
        using var uploader = new UsageLogUploader(client);
        uploader.PublishSession("tester", "gen-1");

        uploader.Record(new UsageLogEntry("Flashing", "x", "success", "evt-1", 1, 2, 100));
        await uploader.FlushAsync();

        handler.Requests.Single().Body.Should().NotContain("details");
    }

    [Fact]
    public async Task Permanent_4xx_rejection_discards_the_batch()
    {
        // 对齐 Rust UploadError::Permanent:服务端明确拒绝(4xx)的批次不能无限重试成毒丸。
        var handler = new RecordingHandler(status: HttpStatusCode.BadRequest);
        var client = new OtaApiClient(new HttpClient(handler), baseUrl: "https://localhost:7243");
        using var uploader = new UsageLogUploader(client);
        uploader.PublishSession("tester", "gen-1");

        uploader.Record(new UsageLogEntry("Flashing", "poison", "success", "evt-1", 1, 2, 100));

        await uploader.FlushAsync();

        handler.Requests.Should().HaveCount(1);
        uploader.PendingCount.Should().Be(0); // 已丢弃,不再重试
    }

    [Fact]
    public async Task Flush_drains_a_large_queue_in_batches_of_one_hundred()
    {
        // 对齐 Rust 的排空循环:250 条 → 100/100/50 三批全部传完,而不是只传当前一批。
        var handler = new RecordingHandler();
        var client = new OtaApiClient(new HttpClient(handler), baseUrl: "https://localhost:7243");
        using var uploader = new UsageLogUploader(client);
        uploader.PublishSession("tester", "gen-1");

        for (var i = 0; i < 250; i++)
        {
            uploader.Record(new UsageLogEntry("Flashing", $"op {i}", "success", $"evt-{i}", i, i, 1));
        }

        await uploader.FlushAsync();

        handler.Requests.Should().HaveCount(3);
        handler.Requests.Select(r => r.Body.Split("\"event_id\"").Length - 1)
            .Should().Equal(100, 100, 50);
        uploader.PendingCount.Should().Be(0);
    }

    [Fact]
    public async Task Durable_queue_survives_a_restart_and_resumes_with_the_same_account()
    {
        // 对齐 Rust:先落盘后上传;进程"重启"(新建 uploader)后同账号续传。
        var path = Path.Combine(spoolDirectory, "usage.json");
        var failing = new RecordingHandler(failNext: true);
        var clientA = new OtaApiClient(new HttpClient(failing), baseUrl: "https://localhost:7243");
        using (var uploader = new UsageLogUploader(clientA, path))
        {
            uploader.PublishSession("account-a", "gen-1");
            uploader.Record(new UsageLogEntry("Flashing", "offline", "success", "evt-1", 1, 2, 100));
            await uploader.FlushAsync();
            uploader.PendingCount.Should().Be(1);
        }

        var resumed = new RecordingHandler();
        var clientB = new OtaApiClient(new HttpClient(resumed), baseUrl: "https://localhost:7243");
        using (var uploader = new UsageLogUploader(clientB, path))
        {
            uploader.PublishSession("account-a", "gen-2"); // 同账号新代次
            uploader.PendingCount.Should().Be(1);
            await uploader.FlushAsync();
            uploader.PendingCount.Should().Be(0);
        }

        resumed.Requests.Should().HaveCount(1);
        resumed.Requests[0].Body.Should().Contain("\"event_id\":\"evt-1\"");
    }

    [Fact]
    public async Task Another_accounts_queue_is_never_uploaded_with_the_current_session()
    {
        // 对齐 Rust:账号 A 未传完的队列绝不能在 B 登录后用 B 的 token 传上去。
        var path = Path.Combine(spoolDirectory, "usage.json");
        var failing = new RecordingHandler(failNext: true);
        var clientA = new OtaApiClient(new HttpClient(failing), baseUrl: "https://localhost:7243");
        using (var uploader = new UsageLogUploader(clientA, path))
        {
            uploader.PublishSession("account-a", "gen-a");
            uploader.Record(new UsageLogEntry("Flashing", "owned-by-a", "success", "evt-a", 1, 2, 100));
            await uploader.FlushAsync();
        }

        var requestsForB = new RecordingHandler();
        var clientB = new OtaApiClient(new HttpClient(requestsForB), baseUrl: "https://localhost:7243");
        using (var uploader = new UsageLogUploader(clientB, path))
        {
            uploader.PublishSession("account-b", "gen-b");
            uploader.PendingCount.Should().Be(1); // 记录还在
            await uploader.FlushAsync();
            requestsForB.Requests.Should().BeEmpty(); // 但不会被 B 的会话上传
        }
    }

    [Fact]
    public async Task Flush_respects_the_deadline_and_keeps_the_queue_durable()
    {
        // 对齐 Rust flush_until:预算耗尽时取消在途请求并保留队列,不阻塞退出。
        var path = Path.Combine(spoolDirectory, "usage.json");
        var handler = new RecordingHandler(delayMs: 10_000);
        var client = new OtaApiClient(new HttpClient(handler), baseUrl: "https://localhost:7243");
        using var uploader = new UsageLogUploader(client, path);
        uploader.PublishSession("account-a", "gen-1");
        uploader.Record(new UsageLogEntry("Flashing", "slow", "success", "evt-1", 1, 2, 100));

        var sw = System.Diagnostics.Stopwatch.StartNew();
        await uploader.FlushAsync(TimeSpan.FromMilliseconds(200));
        sw.Stop();

        sw.Elapsed.Should().BeLessThan(TimeSpan.FromSeconds(5));
        uploader.PendingCount.Should().Be(1); // 保留,已落盘
    }

    [Fact]
    public async Task Stop_drops_new_records_but_keeps_the_existing_queue()
    {
        var handler = new RecordingHandler();
        var client = new OtaApiClient(new HttpClient(handler), baseUrl: "https://localhost:7243");
        using var uploader = new UsageLogUploader(client);
        uploader.PublishSession("tester", "gen-1");

        uploader.Record(new UsageLogEntry("Flashing", "kept", "success", "evt-kept", 1, 2, 100));
        uploader.Stop();
        uploader.Record(new UsageLogEntry("Flashing", "dropped", "success", "evt-dropped", 3, 4, 100));

        await uploader.FlushAsync();

        // Stop 前入队的那条仍被手动 flush 上传并移除;Stop 后的记录被丢弃。
        uploader.PendingCount.Should().Be(0);
        handler.Requests.Single().Body.Should().Contain("evt-kept").And.NotContain("evt-dropped");
    }

    [Fact]
    public async Task CloseSession_flushes_then_stops_accepting_new_records()
    {
        var handler = new RecordingHandler();
        var client = new OtaApiClient(new HttpClient(handler), baseUrl: "https://localhost:7243");
        using var uploader = new UsageLogUploader(client);
        uploader.PublishSession("tester", "gen-1");

        uploader.Record(new UsageLogEntry("Flashing", "before-close", "success", "evt-1", 1, 2, 100));
        await uploader.CloseSessionAsync(TimeSpan.FromSeconds(5));

        handler.Requests.Should().HaveCount(1);
        uploader.PendingCount.Should().Be(0);

        uploader.Record(new UsageLogEntry("Flashing", "after-close", "success", "evt-2", 3, 4, 100));
        uploader.PendingCount.Should().Be(0); // 解绑后不再接收
    }

    [Fact]
    public void Owner_derivation_matches_the_rust_client_and_never_stores_plaintext()
    {
        // 归属标识 = SHA-256(域名分隔符 + 账号):磁盘队列里不会出现明文账号。
        var owner = UsageLogOwner.ForAccount("some-plain-account", "some-plain-generation");
        owner.Account.Should().HaveLength(64).And.NotContain("-");
        owner.Generation.Should().BePositive();

        var again = UsageLogOwner.ForAccount("some-plain-account", "some-plain-generation");
        again.Should().Be(owner);

        UsageLogOwner.ForAccount("some-plain-account", "another-generation")
            .Should().NotBe(owner);
    }

    private sealed class RecordingHandler : HttpMessageHandler
    {
        private readonly bool failNext;
        private readonly int delayMs;
        private readonly HttpStatusCode status;

        public RecordingHandler(bool failNext = false, int delayMs = 0, HttpStatusCode status = HttpStatusCode.OK)
        {
            this.failNext = failNext;
            this.delayMs = delayMs;
            this.status = status;
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
            return new HttpResponseMessage(status)
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
