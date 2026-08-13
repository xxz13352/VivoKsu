using System.IO.Compression;
using FluentAssertions;
using VivoKsu.App.Services;

namespace VivoKsu.App.Tests;

public class FirmwarePartitionExtractorTests
{
    [Theory]
    [InlineData("lk", true)]
    [InlineData("LK", true)]
    [InlineData("preloader", true)]
    [InlineData("preloader_raw", true)]
    [InlineData("preloader_emmc", true)]
    [InlineData("preloader_ufs", true)]
    [InlineData("boot", false)]
    [InlineData("system", false)]
    [InlineData("vendor", false)]
    [InlineData("vbmeta", false)]
    public void ShouldSkip_filters_preloader_and_lk(string partition, bool expected)
    {
        FirmwarePartitionExtractor.ShouldSkip(partition).Should().Be(expected);
    }

    [Fact]
    public async Task ListPartitionsAsync_on_direct_image_zip_filters_preloader_lk_and_non_images()
    {
        var directory = TestDirectories.Create();
        try
        {
            var zip = CreateDirectImageZip(directory, ("boot.img", new byte[] { 0x01 }),
                ("lk.img", new byte[] { 0x02 }),
                ("preloader.img", new byte[] { 0x03 }),
                ("preloader_emmc.img", new byte[] { 0x04 }),
                ("system.new.dat", new byte[100]),
                ("META-INF/com/android/metadata", new byte[] { 0x05 }));
            var extractor = new FirmwarePartitionExtractor(payloadDumper: null);

            var partitions = await extractor.ListPartitionsAsync(zip, CancellationToken.None);

            partitions.Select(partition => partition.Name).Should().BeEquivalentTo(["boot"]);
        }
        finally
        {
            Directory.Delete(directory, recursive: true);
        }
    }

    [Fact]
    public async Task ExtractPartitionAsync_extracts_a_direct_image_entry()
    {
        var directory = TestDirectories.Create();
        try
        {
            var imageBytes = new byte[] { 0x50, 0x4B, 0x03, 0x04, 0xAA, 0xBB };
            var zip = CreateDirectImageZip(directory, ("boot.img", imageBytes), ("lk.img", new byte[] { 0x99 }));
            var extractor = new FirmwarePartitionExtractor(payloadDumper: null);
            var output = Path.Combine(directory, "out");
            Directory.CreateDirectory(output);

            var image = await extractor.ExtractPartitionAsync(zip, "boot", output, CancellationToken.None);

            image.PartitionName.Should().Be("boot");
            image.SizeBytes.Should().Be(imageBytes.Length);
            File.ReadAllBytes(image.ImagePath).Should().BeEquivalentTo(imageBytes);
        }
        finally
        {
            Directory.Delete(directory, recursive: true);
        }
    }

    [Fact]
    public void IsPayloadSource_distinguishes_payload_zip_from_direct_image_zip()
    {
        var directory = TestDirectories.Create();
        try
        {
            var directZip = CreateDirectImageZip(directory, ("boot.img", new byte[] { 0x01 }));
            var payloadZip = CreateZipWithPayload(directory);

            FirmwarePartitionExtractor.IsPayloadSource(directZip).Should().BeFalse();
            FirmwarePartitionExtractor.IsPayloadSource(payloadZip).Should().BeTrue();
        }
        finally
        {
            Directory.Delete(directory, recursive: true);
        }
    }

    private static string CreateDirectImageZip(string directory, params (string Name, byte[] Content)[] entries)
    {
        var path = Path.Combine(directory, "ota.zip");
        using (var archive = ZipFile.Open(path, ZipArchiveMode.Create))
        {
            foreach (var (name, content) in entries)
            {
                var entry = archive.CreateEntry(name);
                using var stream = entry.Open();
                stream.Write(content);
            }
        }

        return path;
    }

    private static string CreateZipWithPayload(string directory)
    {
        var path = Path.Combine(directory, "payload_ota.zip");
        using (var archive = ZipFile.Open(path, ZipArchiveMode.Create))
        {
            var entry = archive.CreateEntry("payload.bin");
            using var stream = entry.Open();
            stream.Write([0x43, 0x72, 0x41, 0x55]); // CrAU
        }

        return path;
    }

    private static class TestDirectories
    {
        public static string Create()
        {
            var path = Path.Combine(
                Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
            Directory.CreateDirectory(path);
            return path;
        }
    }
}
