using System.IO.Compression;
using System.Net;
using System.Net.Http;
using VivoKsu.App.Services;

namespace VivoKsu.App.Tests;

public class ScrcpyProvisioningServiceTests
{
    [Fact]
    public async Task EnsureInstalledAsync_downloads_the_official_windows_asset_and_extracts_scrcpy()
    {
        var root = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        var assetUri = new Uri("https://github.com/Genymobile/scrcpy/releases/download/v3.3/scrcpy-win64-v3.3.zip");
        using var client = new HttpClient(new ScrcpyReleaseHandler(assetUri, CreateScrcpyArchive()));

        try
        {
            var serviceType = typeof(MirrorService).Assembly.GetType("VivoKsu.App.Services.ScrcpyProvisioningService");
            Assert.NotNull(serviceType);
            var service = Activator.CreateInstance(serviceType!, [client, root, null]);
            Assert.NotNull(service);
            var ensureInstalled = serviceType!.GetMethod("EnsureInstalledAsync");
            Assert.NotNull(ensureInstalled);

            var task = Assert.IsAssignableFrom<Task<string>>(ensureInstalled!.Invoke(service, [CancellationToken.None, null]));
            var executable = await task;

            Assert.True(File.Exists(executable));
            Assert.EndsWith("scrcpy.exe", executable, StringComparison.OrdinalIgnoreCase);
            Assert.Equal("scrcpy-binary", await File.ReadAllTextAsync(executable));
        }
        finally
        {
            if (Directory.Exists(root))
            {
                DeleteTemporaryDirectory(root);
            }
        }
    }

    private static void DeleteTemporaryDirectory(string path)
    {
        for (var attempt = 0; attempt < 5; attempt++)
        {
            try
            {
                Directory.Delete(path, true);
                return;
            }
            catch (IOException) when (attempt < 4)
            {
                Thread.Sleep(100);
            }
        }
    }

    private static byte[] CreateScrcpyArchive()
    {
        using var stream = new MemoryStream();
        using (var archive = new ZipArchive(stream, ZipArchiveMode.Create, leaveOpen: true))
        {
            using var writer = new StreamWriter(archive.CreateEntry("scrcpy-win64-v3.3/scrcpy.exe").Open());
            writer.Write("scrcpy-binary");
        }

        return stream.ToArray();
    }

    private sealed class ScrcpyReleaseHandler(Uri assetUri, byte[] archive) : HttpMessageHandler
    {
        protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken)
        {
            if (request.RequestUri!.AbsolutePath.EndsWith("/releases/latest", StringComparison.Ordinal))
            {
                var json = $$"""{"assets":[{"name":"scrcpy-win64-v3.3.zip","browser_download_url":"{{assetUri}}"}]}""";
                return Task.FromResult(new HttpResponseMessage(HttpStatusCode.OK)
                {
                    Content = new StringContent(json)
                });
            }

            if (request.RequestUri == assetUri)
            {
                return Task.FromResult(new HttpResponseMessage(HttpStatusCode.OK)
                {
                    Content = new ByteArrayContent(archive)
                });
            }

            return Task.FromResult(new HttpResponseMessage(HttpStatusCode.NotFound));
        }
    }
}
