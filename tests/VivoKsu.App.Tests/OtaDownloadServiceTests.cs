using Downloader;
using FluentAssertions;
using VivoKsu.App.Services;

namespace VivoKsu.App.Tests;

public class OtaDownloadServiceTests
{
    [Fact]
    public void MapProgress_maps_the_library_event_to_download_progress()
    {
        var args = new DownloadProgressChangedEventArgs("id")
        {
            ReceivedBytesSize = 100,
            TotalBytesToReceive = 200,
            BytesPerSecondSpeed = 5_000_000
        };

        var progress = OtaDownloadService.MapProgress(args);

        progress.DownloadedBytes.Should().Be(100);
        progress.TotalBytes.Should().Be(200);
        progress.BytesPerSecond.Should().Be(5_000_000);
    }

    [Fact]
    public void MapProgress_omits_the_total_when_the_library_does_not_know_it()
    {
        var args = new DownloadProgressChangedEventArgs("id")
        {
            ReceivedBytesSize = 100,
            TotalBytesToReceive = 0,
            BytesPerSecondSpeed = 1
        };

        OtaDownloadService.MapProgress(args).TotalBytes.Should().BeNull();
    }

    [Fact]
    public void DownloadAsync_rejects_invalid_arguments()
    {
        using var downloader = new OtaDownloadService();

        Action nullUrl = () => downloader.DownloadAsync(null!, @"C:\x\y.bin", null, CancellationToken.None);
        Action emptyPath = () => downloader.DownloadAsync(new Uri("https://cdn.example/full.zip"), "", null, CancellationToken.None);

        nullUrl.Should().Throw<ArgumentNullException>();
        emptyPath.Should().Throw<ArgumentException>();
    }

    [Fact]
    public void BuildConfiguration_sets_full_range_when_server_supports_range()
    {
        // 回归:bezzad 5.9.5 开 RangeDownload 但 RangeHigh 为 0 时会把
        // TotalFileSize 算成 1,导致只下载 1 字节。这里必须设 RangeHigh=总大小-1。
        var remote = new RemoteFileInfo { FileSize = 8_340_325_251, SupportsRange = true };

        var config = OtaDownloadService.BuildConfiguration(remote);

        config.RangeDownload.Should().BeTrue();
        config.RangeLow.Should().Be(0);
        config.RangeHigh.Should().Be(8_340_325_250);
        config.ChunkCount.Should().Be(8);
    }

    [Fact]
    public void BuildConfiguration_uses_single_connection_when_server_does_not_support_range()
    {
        var remote = new RemoteFileInfo { FileSize = 1_000, SupportsRange = false };

        var config = OtaDownloadService.BuildConfiguration(remote);

        config.RangeDownload.Should().BeFalse();
        config.ChunkCount.Should().Be(1);
    }

    [Fact]
    public void EnsureDiskSpace_throws_when_disk_is_too_small()
    {
        var path = Path.Combine(Path.GetTempPath(), "ota.bin");

        Action check = () => OtaDownloadService.EnsureDiskSpace(path, long.MaxValue / 2);

        check.Should().Throw<IOException>().WithMessage("*磁盘空间不足*");
    }

    [Fact]
    public void EnsureDiskSpace_passes_when_disk_has_enough_room()
    {
        var path = Path.Combine(Path.GetTempPath(), "ota.bin");

        Action check = () => OtaDownloadService.EnsureDiskSpace(path, 1);

        check.Should().NotThrow();
    }
}
