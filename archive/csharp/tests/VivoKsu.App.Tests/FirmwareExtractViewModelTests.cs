using System.Text;
using FluentAssertions;
using VivoKsu.App.Models;
using VivoKsu.App.Services;
using VivoKsu.App.ViewModels;

namespace VivoKsu.App.Tests;

public class FirmwareExtractViewModelTests
{
    [Fact]
    public async Task ReadInfoAsync_populates_partitions_from_a_local_payload()
    {
        var viewModel = CreateViewModel(out _);
        if (!viewModel.IsPayloadToolAvailable)
        {
            return;
        }

        var payload = CreatePayloadFile();
        viewModel.PayloadSourceUrl = payload;

        await viewModel.ReadInfoCommand.ExecuteAsync(null);

        viewModel.PayloadPartitions.Select(partition => partition.Name)
            .Should().BeEquivalentTo("boot", "init_boot", "vendor_boot");
        viewModel.PayloadStatusText.Should().Contain("3 个分区");
    }

    [Fact]
    public async Task ExtractAsync_writes_selected_images_and_raises_has_extracted()
    {
        var viewModel = CreateViewModel(out var outputDirectory);
        if (!viewModel.IsPayloadToolAvailable)
        {
            return;
        }

        var payload = CreatePayloadFile();
        viewModel.PayloadSourceUrl = payload;
        await viewModel.ReadInfoCommand.ExecuteAsync(null);
        viewModel.PayloadPartitions.Single(partition => partition.Name == "boot").IsSelected = true;
        viewModel.OutputPath = outputDirectory;

        await viewModel.ExtractCommand.ExecuteAsync(null);

        viewModel.HasExtractedImages.Should().BeTrue();
        File.Exists(Path.Combine(outputDirectory, "boot.img")).Should().BeTrue();
        viewModel.PayloadProgress.Should().Be(1.0);
        viewModel.SpeedText.Should().NotBe("--");
        viewModel.ElapsedText.Should().NotBe("00:00");
    }

    [Fact]
    public async Task MapToQuickFlash_invokes_the_continuation_for_preset_partitions()
    {
        var viewModel = CreateViewModel(out var outputDirectory);
        if (!viewModel.IsPayloadToolAvailable)
        {
            return;
        }

        var mappedImages = new List<FlashImageInfo>();
        viewModel.SetFlashContinuation((image, _) => mappedImages.Add(image));

        var payload = CreatePayloadFile();
        viewModel.PayloadSourceUrl = payload;
        await viewModel.ReadInfoCommand.ExecuteAsync(null);
        viewModel.PayloadPartitions.Single(partition => partition.Name == "boot").IsSelected = true;
        viewModel.OutputPath = outputDirectory;
        await viewModel.ExtractCommand.ExecuteAsync(null);

        viewModel.MapToQuickFlashCommand.Execute(null);

        mappedImages.Should().HaveCount(1);
    }

    [Fact]
    public async Task ReadInfoAsync_twice_does_not_delete_a_local_vivo_gzip_source()
    {
        var directory = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(directory);
        var gzipPath = Path.Combine(directory, "vivo_ota.gz");
        File.WriteAllBytes(gzipPath, [0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff]);
        try
        {
            var viewModel = new FirmwareExtractViewModel(
                new OperationLogService(),
                payloadDumper: new PayloadDumperRunner(
                    Path.Combine(AppContext.BaseDirectory, "payload-tools", "payload_dumper.exe")),
                vivoExtractor: new VivoFirmwareExtractor());
            viewModel.PayloadSourceUrl = gzipPath;

            // 第一次读取:本地源 PrepareGzipAsync 原样返回路径,CachedPreparedGzip 指向用户自己的文件。
            await viewModel.ReadInfoCommand.ExecuteAsync(null);
            File.Exists(gzipPath).Should().BeTrue();

            // 第二次读取:CleanupPreparedGzip 只应删除应用下载的临时 gzip,绝不能删本地固件。
            await viewModel.ReadInfoCommand.ExecuteAsync(null);
            File.Exists(gzipPath).Should().BeTrue("CleanupPreparedGzip 不得删除用户的本地固件文件");
        }
        finally
        {
            Directory.Delete(directory, recursive: true);
        }
    }

    private static FirmwareExtractViewModel CreateViewModel(out string outputDirectory)
    {
        var logs = new OperationLogService();
        var runner = new PayloadDumperRunner(
            Path.Combine(AppContext.BaseDirectory, "payload-tools", "payload_dumper.exe"));
        outputDirectory = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        return new FirmwareExtractViewModel(logs, runner);
    }

    private static string CreatePayloadFile()
    {
        var partitions = new[]
        {
            ("boot", BuildPattern(2048)),
            ("init_boot", Encoding.ASCII.GetBytes(string.Concat(Enumerable.Repeat("INITBOOT", 128)))),
            ("vendor_boot", Encoding.ASCII.GetBytes(string.Concat(Enumerable.Repeat("VENDORBOOT", 200))))
        };
        var payload = PayloadTestData.Build(partitions);
        var path = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", $"{Guid.NewGuid():N}.bin");
        Directory.CreateDirectory(Path.GetDirectoryName(path)!);
        File.WriteAllBytes(path, payload);
        return path;
    }

    private static byte[] BuildPattern(int length)
    {
        var data = new byte[length];
        for (var i = 0; i < length; i++)
        {
            data[i] = (byte)i;
        }

        return data;
    }
}
