using System.IO.Compression;
using System.Net;
using VivoKsu.App.Services;

namespace VivoKsu.App.Tests;

public class PayloadDumperProvisionerTests
{
    [Fact]
    public async Task Uses_bundled_executable_when_present()
    {
        var bundled = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"), "payload_dumper.exe");
        Directory.CreateDirectory(Path.GetDirectoryName(bundled)!);
        File.WriteAllText(bundled, "bundled-dummy");

        try
        {
            var provisioner = new PayloadDumperProvisioner(bundledExecutablePath: bundled);
            Assert.True(provisioner.IsAvailable);
            Assert.Equal(bundled, provisioner.ExecutablePath);

            var runner = await provisioner.EnsureInstalledAsync(CancellationToken.None);
            Assert.True(runner.IsAvailable);
        }
        finally
        {
            TryDeleteDirectory(Path.GetDirectoryName(bundled)!);
        }
    }

    [Fact]
    public async Task Downloads_verifies_and_caches_when_no_bundled_or_cached()
    {
        // 用真实 payload_dumper.exe 打 zip 作为下载内容:其哈希与 RemoteAssetCatalog pin 一致,下载后应校验通过。
        var realExecutable = Path.Combine(AppContext.BaseDirectory, "payload-tools", "payload_dumper.exe");
        Assert.True(File.Exists(realExecutable), "测试需要随测试输出复制的真实 payload_dumper.exe。");
        var realBytes = await File.ReadAllBytesAsync(realExecutable);
        var zipBytes = ZipSingleFile(RemoteAssetCatalog.PayloadDumperExecutableName, realBytes);

        using var handler = new TestRoutingHandler().Route("github.com", HttpStatusCode.OK, zipBytes);
        using var client = new HttpClient(handler);
        var downloader = new RemoteAssetDownloader(client, mirrorList: []);
        var root = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));

        try
        {
            var provisioner = new PayloadDumperProvisioner(downloader, installationRoot: root);
            var runner = await provisioner.EnsureInstalledAsync(CancellationToken.None);

            Assert.True(provisioner.IsAvailable);
            Assert.True(runner.IsAvailable);
            var cached = Path.Combine(root, RemoteAssetCatalog.PayloadDumperExecutableName);
            Assert.True(File.Exists(cached));
            Assert.Equal(realBytes, await File.ReadAllBytesAsync(cached));
        }
        finally
        {
            TryDeleteDirectory(root);
        }
    }

    [Fact]
    public async Task Reuses_cached_executable_without_downloading_again()
    {
        var realExecutable = Path.Combine(AppContext.BaseDirectory, "payload-tools", "payload_dumper.exe");
        Assert.True(File.Exists(realExecutable));
        var root = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(root);
        File.Copy(realExecutable, Path.Combine(root, RemoteAssetCatalog.PayloadDumperExecutableName));

        try
        {
            // 任何网络请求都不该发生:缓存已存在。
            using var handler = new TestRoutingHandler();
            using var client = new HttpClient(handler);
            var provisioner = new PayloadDumperProvisioner(
                new RemoteAssetDownloader(client, mirrorList: []), installationRoot: root);

            var runner = await provisioner.EnsureInstalledAsync(CancellationToken.None);
            Assert.True(runner.IsAvailable);
            Assert.Empty(handler.Requests);
        }
        finally
        {
            TryDeleteDirectory(root);
        }
    }

    [Fact]
    public async Task Rejects_payload_whose_extracted_executable_hash_mismatches()
    {
        var zipBytes = ZipSingleFile(RemoteAssetCatalog.PayloadDumperExecutableName, new byte[] { 1, 2, 3, 4 });

        using var handler = new TestRoutingHandler().Route("github.com", HttpStatusCode.OK, zipBytes);
        using var client = new HttpClient(handler);
        var downloader = new RemoteAssetDownloader(client, mirrorList: []);
        var root = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));

        try
        {
            var provisioner = new PayloadDumperProvisioner(downloader, installationRoot: root);
            await Assert.ThrowsAsync<InvalidDataException>(() => provisioner.EnsureInstalledAsync(CancellationToken.None));
            Assert.False(provisioner.IsAvailable);
        }
        finally
        {
            TryDeleteDirectory(root);
        }
    }

    private static byte[] ZipSingleFile(string entryName, byte[] content)
    {
        using var stream = new MemoryStream();
        using (var archive = new ZipArchive(stream, ZipArchiveMode.Create, leaveOpen: true))
        {
            var entry = archive.CreateEntry(entryName, CompressionLevel.Optimal);
            using var entryStream = entry.Open();
            entryStream.Write(content);
        }

        return stream.ToArray();
    }

    private static void TryDeleteDirectory(string path)
    {
        try
        {
            if (Directory.Exists(path))
            {
                Directory.Delete(path, true);
            }
        }
        catch
        {
            // Best effort.
        }
    }
}
