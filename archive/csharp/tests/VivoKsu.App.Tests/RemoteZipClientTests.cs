using System.IO;
using System.IO.Compression;
using System.Net;
using System.Net.Http;
using System.Text;
using FluentAssertions;
using VivoKsu.App.Models;
using VivoKsu.App.Services;

namespace VivoKsu.App.Tests;

/// <summary>
/// 远程 ZIP 流式读取 + ROOT 云端提取的全链路测试:
/// 内存里构造真实 ZIP,用只支持 Range 的模拟服务器(206/Content-Range)承载,
/// 验证「只下载所需字节」的完整语义 —— 建档/列成员/按需解压/CRC 校验/部分回退。
/// </summary>
[CollectionDefinition(nameof(SequencedCollection), DisableParallelization = true)]
public sealed class SequencedCollection;

[Collection(nameof(SequencedCollection))]
public class RemoteZipClientTests : IDisposable
{
    private readonly string outputDir = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", $"rzip-{Guid.NewGuid():N}");

    public void Dispose()
    {
        try
        {
            if (Directory.Exists(outputDir))
            {
                Directory.Delete(outputDir, recursive: true);
            }
        }
        catch
        {
            // 临时目录清不掉不影响测试结论。
        }
    }

    [Fact]
    public async Task ListMembers_reads_the_central_directory_without_downloading_the_whole_zip()
    {
        var zip = TestZip.Create(("boot.img", RandomBytes(300_000)), ("vendor_boot.img", RandomBytes(200_000)), ("readme.txt", "hello"u8.ToArray()));
        using var server = new RangeZipServer(zip);
        using var http = server.CreateClient();

        var members = await RemoteZipClient.ListMembersAsync(http, server.Url);

        members.Select(m => m.Name).Should().Equal("boot", "vendor_boot", "readme");
        members.First(m => m.Name == "boot").SizeBytes.Should().Be(300_000);
        // 建档只需要 EOCD + 中央目录,远小于整包。
        server.BytesServed.Should().BeLessThan((long)zip.Length);
    }

    [Fact]
    public async Task ExtractMembers_downloads_only_wanted_partitions_and_verifies_content()
    {
        var bootBytes = RandomBytes(400_000);
        var vendorBytes = RandomBytes(250_000);
        var zip = TestZip.Create(("init_boot.img", bootBytes), ("vendor_boot.img", vendorBytes), ("system.img", RandomBytes(1_000_000)));
        using var server = new RangeZipServer(zip);
        using var http = server.CreateClient();

        var extracted = await RemoteZipClient.ExtractMembersAsync(http, server.Url, ["init_boot", "vendor_boot"], outputDir);

        extracted.Select(e => e.PartitionName).Should().Equal("init_boot", "vendor_boot");
        File.ReadAllBytes(extracted[0].Path).Should().Equal(bootBytes); // CRC/内容一致
        File.ReadAllBytes(extracted[1].Path).Should().Equal(vendorBytes);
        extracted[0].SizeBytes.Should().Be(bootBytes.Length);
        // 不应把没要的 system.img 或整包拉下来。
        File.Exists(Path.Combine(outputDir, "system.img")).Should().BeFalse();
        server.BytesServed.Should().BeLessThan(bootBytes.Length + vendorBytes.Length + (long)zip.Length / 2);
        Directory.GetFiles(outputDir, "*.partial", SearchOption.AllDirectories).Should().BeEmpty();
    }

    [Fact]
    public async Task ExtractMembers_returns_null_partition_when_absent()
    {
        var zip = TestZip.Create(("boot.img", RandomBytes(10_000)));
        using var server = new RangeZipServer(zip);
        using var http = server.CreateClient();

        var extracted = await RemoteZipClient.ExtractMembersAsync(http, server.Url, ["init_boot", "boot"], outputDir);

        extracted.Select(e => e.PartitionName).Should().Equal("boot"); // init_boot 缺失不报错,只跳过
    }

    [Fact]
    public async Task ExtractMembers_fails_when_the_member_data_is_corrupted()
    {
        // 篡改成员压缩数据:显式 CRC 校验必须失败。
        // (.NET 8 的 ZipArchive Read 模式不校验 CRC —— 已实测,这正是自研解析+显式校验的原因。)
        var zip = TestZip.Create(("boot.img", RandomBytes(100_000)));
        CorruptMemberData(zip, "boot.img", 50_000, 64);
        using var server = new RangeZipServer(zip);
        using var http = server.CreateClient();

        var act = () => RemoteZipClient.ExtractMembersAsync(http, server.Url, ["boot"], outputDir);

        await act.Should().ThrowAsync<IOException>();
        File.Exists(Path.Combine(outputDir, "boot.img")).Should().BeFalse(); // 损坏产物不得落盘
    }

    [Fact]
    public async Task ExtractMembers_fails_when_the_central_crc_was_tampered()
    {
        // 翻转中央目录 CRC 记录:数据正常也必须判失败(元数据不可信场景)。
        var zip = TestZip.Create(("boot.img", RandomBytes(100_000)));
        CorruptCentralCrc(zip, "boot.img");
        using var server = new RangeZipServer(zip);
        using var http = server.CreateClient();

        var act = () => RemoteZipClient.ExtractMembersAsync(http, server.Url, ["boot"], outputDir);

        (await act.Should().ThrowAsync<IOException>())
            .Which.Message.Should().Contain("CRC");
    }

    [Fact]
    public async Task RemoteRangeStream_rejects_servers_without_range_support()
    {
        var zip = TestZip.Create(("boot.img", RandomBytes(10_000)));
        // 只返回 200 全量的服务器:必须显式失败,不允许悄悄退化成整包下载。
        using var server = new RangeZipServer(zip, supportRange: false);
        using var http = server.CreateClient();

        var act = () => RemoteZipClient.ListMembersAsync(http, server.Url);

        (await act.Should().ThrowAsync<IOException>())
            .Which.Message.Should().Contain("分段下载");
    }

    [Fact]
    public void ValidateUrl_rejects_non_http_schemes()
    {
        var act = () => RemoteRangeStream.ValidateUrl("ftp://example.com/ota.zip");
        act.Should().Throw<ArgumentException>();
        var act2 = () => RemoteRangeStream.ValidateUrl("not a url");
        act2.Should().Throw<ArgumentException>();
        RemoteRangeStream.ValidateUrl("https://example.com/ota.zip").Should().Be("https://example.com/ota.zip");
    }

    [Fact]
    public async Task RootOtaCloudExtract_prefers_init_boot_over_boot()
    {
        var initBoot = RandomBytes(50_000);
        var boot = RandomBytes(60_000);
        var vendor = RandomBytes(70_000);
        var zip = TestZip.Create(("init_boot.img", initBoot), ("boot.img", boot), ("vendor_boot.img", vendor));
        using var server = new RangeZipServer(zip);

        // OtaApiClient 走注入 handler 返回固定 ROM 记录;提取流量走同一个 server。
        var apiHandler = new RomResolveHandler(server.Url);
        var client = new OtaApiClient(new HttpClient(apiHandler), baseUrl: "https://localhost:7243");
        var service = new RootOtaCloudExtractService(client, backend: null, http: server.CreateClient());

        var result = await service.ExtractAsync("PD2057", "15.2.12.0", outputDir, stage => { }, CancellationToken.None);

        result.BootPartitionName.Should().Be("init_boot"); // init_boot 优先
        result.BootImage!.Path.Should().EndWith("init_boot.img");
        File.ReadAllBytes(result.BootImage!.Path).Should().Equal(initBoot);
        result.VendorBoot!.Path.Should().EndWith("vendor_boot.img");
        File.ReadAllBytes(result.VendorBoot!.Path).Should().Equal(vendor);
        result.StagingDirectory.Should().Contain("root-ota-");
        // OTA URL 不得出现在异常外的任何回传字段里(对齐 Rust「URL 不进浏览器」)。
        result.ToString().Should().NotContain(server.Url);
    }

    private static byte[] RandomBytes(int length)
    {
        var bytes = new byte[length];
        Random.Shared.NextBytes(bytes);
        return bytes;
    }

    /// <summary>篡改 ZIP 内指定成员的压缩数据区(解压后 CRC 必然不匹配)。</summary>
    private static void CorruptMemberData(byte[] zip, string memberName, int offsetInData, int count)
    {
        var nameBytes = Encoding.UTF8.GetBytes(memberName);
        for (var i = 0; i < zip.Length - 30 - nameBytes.Length; i++)
        {
            if (zip[i] == 0x50 && zip[i + 1] == 0x4B && zip[i + 2] == 0x03 && zip[i + 3] == 0x04
                && zip.AsSpan(i + 30, nameBytes.Length).SequenceEqual(nameBytes))
            {
                var extraLength = BitConverter.ToUInt16(zip, i + 28);
                var dataStart = i + 30 + nameBytes.Length + extraLength;
                for (var k = 0; k < count; k++)
                {
                    zip[dataStart + offsetInData + k] ^= 0xFF;
                }

                return;
            }
        }

        throw new InvalidOperationException("未找到成员的 local header。");
    }

    /// <summary>翻转中央目录里记录的 CRC-32(模拟包元数据不可信)。</summary>
    private static void CorruptCentralCrc(byte[] zip, string memberName)
    {
        var nameBytes = Encoding.UTF8.GetBytes(memberName);
        for (var i = 0; i < zip.Length - 46 - nameBytes.Length; i++)
        {
            if (zip[i] == 0x50 && zip[i + 1] == 0x4B && zip[i + 2] == 0x01 && zip[i + 3] == 0x02
                && zip.AsSpan(i + 46, nameBytes.Length).SequenceEqual(nameBytes))
            {
                for (var b = 0; b < 4; b++)
                {
                    zip[i + 16 + b] ^= 0xFF;
                }

                return;
            }
        }

        throw new InvalidOperationException("未找到成员的 central header。");
    }

    /// <summary>构造 ZIP 的辅助(与被测代码同用 ZipArchive,保证是合规包)。</summary>
    private static class TestZip
    {
        public static byte[] Create(params (string Name, byte[] Data)[] members)
        {
            using var output = new MemoryStream();
            using (var archive = new ZipArchive(output, ZipArchiveMode.Create, leaveOpen: true))
            {
                foreach (var (name, data) in members)
                {
                    var entry = archive.CreateEntry(name, CompressionLevel.Optimal);
                    using var stream = entry.Open();
                    stream.Write(data);
                }
            }

            return output.ToArray();
        }
    }

    /// <summary>
    /// 承载内存 ZIP 的模拟服务器:严格按 RFC 7233 返回 206 + Content-Range,
    /// 并统计实际发出的字节数(验证「只下载所需字节」)。
    /// </summary>
    private sealed class RangeZipServer : IDisposable
    {
        private readonly byte[] zip;
        private readonly bool supportRange;
        private readonly HttpListener listener = new();
        private long bytesServed;

        public RangeZipServer(byte[] zip, bool supportRange = true)
        {
            this.zip = zip;
            this.supportRange = supportRange;
            // 用 127.0.0.1 的随机高位端口,避免测试间冲突。
            Port = 49_000 + Random.Shared.Next(0, 5_000);
            listener.Prefixes.Add($"http://127.0.0.1:{Port}/");
            listener.Start();
            _ = Task.Run(AcceptLoopAsync);
        }

        public string Url => $"http://127.0.0.1:{Port}/ota.zip";

        public int Port { get; }

        public long BytesServed => Interlocked.Read(ref bytesServed);

        public HttpClient CreateClient() => new(new NoRedirectHandler());

        /// <summary>下一次请求(测试串行,单个 pending 即够)。</summary>
        private void Handle(HttpListenerContext context)
        {
            try
            {
                var range = context.Request.Headers["Range"];
                int start = 0, end = zip.Length - 1;
                var isRange = false;
                if (supportRange && range is not null && range.StartsWith("bytes=", StringComparison.Ordinal))
                {
                    var parts = range["bytes=".Length..].Split('-');
                    start = int.Parse(parts[0]);
                    if (parts.Length > 1 && parts[1].Length > 0)
                    {
                        end = int.Parse(parts[1]);
                    }

                    isRange = true;
                }

                var length = end - start + 1;
                if (isRange)
                {
                    context.Response.StatusCode = 206;
                    context.Response.ContentType = "application/octet-stream";
                    context.Response.SendChunked = false;
                    context.Response.Headers["Content-Range"] = $"bytes {start}-{end}/{zip.Length}";
                    context.Response.ContentLength64 = length;
                    context.Response.OutputStream.Write(zip, start, length);
                    context.Response.OutputStream.Close();
                }
                else
                {
                    context.Response.StatusCode = 200;
                    context.Response.ContentLength64 = zip.Length;
                    context.Response.OutputStream.Write(zip, 0, zip.Length);
                    context.Response.OutputStream.Close();
                }

                Interlocked.Add(ref bytesServed, length);
            }
            catch
            {
                // 客户端取消/断开:忽略,继续等下一个请求。
            }
        }

        /// <summary>循环接受请求:每个请求独立处理,响应写在循环内完成后再接下一个(时序确定)。</summary>
        private async Task AcceptLoopAsync()
        {
            while (listener.IsListening)
            {
                HttpListenerContext context;
                try
                {
                    context = await listener.GetContextAsync();
                }
                catch
                {
                    // listener 已停止:退出循环。
                    return;
                }

                _ = Task.Run(() => Handle(context));
            }
        }

        public void Dispose()
        {
            try
            {
                listener.Stop();
                listener.Close();
            }
            catch
            {
                // 已停止:忽略。
            }
        }
    }

    private sealed class NoRedirectHandler : HttpClientHandler
    {
        public NoRedirectHandler() => AllowAutoRedirect = false;

        protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken)
            => base.SendAsync(request, cancellationToken);
    }

    /// <summary>把 /api/rom 请求指到测试 server 的 ROM 解析桩。</summary>
    private sealed class RomResolveHandler : HttpMessageHandler
    {
        private readonly string romUrl;

        public RomResolveHandler(string romUrl) => this.romUrl = romUrl;

        protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken)
        {
            var body = $$"""{"pd":"PD2057","version":"15.2.12.0","url":"{{romUrl}}","name":"ota.zip","sizeBytes":1}""";
            return Task.FromResult(new HttpResponseMessage(HttpStatusCode.OK)
            {
                Content = new StringContent(body, Encoding.UTF8, "application/json"),
            });
        }
    }
}
