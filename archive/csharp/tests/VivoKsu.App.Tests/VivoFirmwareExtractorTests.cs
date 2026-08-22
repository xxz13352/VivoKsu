using System.IO.Compression;
using System.Text;
using FluentAssertions;
using VivoKsu.App.Services;

namespace VivoKsu.App.Tests;

public class VivoFirmwareExtractorTests
{
    [Fact]
    public async Task ListAsync_parses_gzip_tar_entries_including_long_names()
    {
        var gzipPath = CreateGzipTar(new[]
        {
            ("boot.img", new byte[] { 1, 2, 3, 4 }),
            ("DPD2329_A_14.0.10.1.W10.V000L1_vivo_mtk_alps-release-u0.bsp/super.img", Enumerable.Range(0, 10000).Select(i => (byte)(i % 256)).ToArray())
        });

        var extractor = new VivoFirmwareExtractor();
        var entries = await extractor.ListAsync(gzipPath, null, CancellationToken.None);

        entries.Select(entry => entry.Name).Should().BeEquivalentTo("boot.img", "super.img");
        entries.Should().ContainSingle(entry => entry.Name == "super.img" && entry.SizeBytes == 10000);
    }

    [Fact]
    public async Task ExtractAsync_extracts_selected_entries_with_progress()
    {
        var gzipPath = CreateGzipTar(new[]
        {
            ("boot.img", new byte[] { 9, 8, 7, 6 }),
            ("vendor.img", new byte[] { 1, 1, 2, 2, 3, 3 })
        });
        var extractor = new VivoFirmwareExtractor();
        var entries = await extractor.ListAsync(gzipPath, null, CancellationToken.None);
        var outputDirectory = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        var progress = new List<VivoFirmwareExtractor.VivoProgress>();

        var results = await extractor.ExtractAsync(
            gzipPath,
            entries.Where(entry => entry.Name == "boot.img").ToArray(),
            outputDirectory,
            new SyncProgress<VivoFirmwareExtractor.VivoProgress>(progress.Add),
            CancellationToken.None);

        results.Should().HaveCount(1);
        File.ReadAllBytes(Path.Combine(outputDirectory, "boot.img")).Should().Equal(new byte[] { 9, 8, 7, 6 });
        progress.Should().NotBeEmpty();
        progress.Last().Fraction.Should().BeGreaterThan(0);
    }

    [Fact]
    public async Task ExtractAsync_rejects_a_truncated_selected_entry_without_replacing_an_existing_output()
    {
        var gzipPath = CreateTruncatedGzipTar("boot.img", new byte[] { 1, 2, 3, 4 });
        var outputDirectory = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(outputDirectory);
        var outputPath = Path.Combine(outputDirectory, "boot.img");
        await File.WriteAllBytesAsync(outputPath, new byte[] { 9, 9, 9 });
        var extractor = new VivoFirmwareExtractor();

        var action = () => extractor.ExtractAsync(
            gzipPath,
            [new VivoFirmwareExtractor.VivoFirmwareEntry("boot.img", "boot.img", 4)],
            outputDirectory,
            null,
            CancellationToken.None);

        await action.Should().ThrowAsync<InvalidDataException>();
        (await File.ReadAllBytesAsync(outputPath)).Should().Equal(new byte[] { 9, 9, 9 });
        Directory.EnumerateFiles(outputDirectory, "*.partial").Should().BeEmpty();
    }

    [Fact]
    public async Task ExtractAsync_does_not_publish_any_selected_entry_when_a_later_entry_is_truncated()
    {
        var first = (Name: "boot.img", Data: new byte[] { 1, 2, 3, 4 });
        var second = (Name: "vendor.img", Data: new byte[] { 5, 6, 7, 8 });
        var gzipPath = CreateGzipTarWithTruncatedSecondEntry(first, second);
        var outputDirectory = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(outputDirectory);
        var bootPath = Path.Combine(outputDirectory, first.Name);
        var vendorPath = Path.Combine(outputDirectory, second.Name);
        await File.WriteAllBytesAsync(bootPath, new byte[] { 9, 9, 9 });
        await File.WriteAllBytesAsync(vendorPath, new byte[] { 8, 8, 8 });
        var extractor = new VivoFirmwareExtractor();

        var action = () => extractor.ExtractAsync(
            gzipPath,
            [
                new VivoFirmwareExtractor.VivoFirmwareEntry(first.Name, first.Name, first.Data.Length),
                new VivoFirmwareExtractor.VivoFirmwareEntry(second.Name, second.Name, second.Data.Length)
            ],
            outputDirectory,
            null,
            CancellationToken.None);

        await action.Should().ThrowAsync<InvalidDataException>();
        (await File.ReadAllBytesAsync(bootPath)).Should().Equal(new byte[] { 9, 9, 9 });
        (await File.ReadAllBytesAsync(vendorPath)).Should().Equal(new byte[] { 8, 8, 8 });
        Directory.EnumerateFiles(outputDirectory, "*.partial").Should().BeEmpty();
    }

    private static string CreateGzipTar((string Name, byte[] Data)[] files)
    {
        var tar = BuildTar(files);
        var gzipPath = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", $"{Guid.NewGuid():N}.gz");
        Directory.CreateDirectory(Path.GetDirectoryName(gzipPath)!);
        using (var file = File.Create(gzipPath))
        using (var gzip = new GZipStream(file, CompressionMode.Compress))
        {
            gzip.Write(tar);
        }

        return gzipPath;
    }

    private static string CreateTruncatedGzipTar(string name, byte[] data)
    {
        var tar = BuildTar([(name, data)]);
        var gzipPath = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", $"{Guid.NewGuid():N}.gz");
        Directory.CreateDirectory(Path.GetDirectoryName(gzipPath)!);
        using (var file = File.Create(gzipPath))
        using (var gzip = new GZipStream(file, CompressionMode.Compress))
        {
            gzip.Write(tar, 0, 512 + data.Length - 1);
        }

        return gzipPath;
    }

    private static string CreateGzipTarWithTruncatedSecondEntry(
        (string Name, byte[] Data) first,
        (string Name, byte[] Data) second)
    {
        var tar = BuildTar([first, second]);
        var secondPayloadOffset = 512 + ((first.Data.Length + 511) / 512 * 512) + 512;
        var gzipPath = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", $"{Guid.NewGuid():N}.gz");
        Directory.CreateDirectory(Path.GetDirectoryName(gzipPath)!);
        using (var file = File.Create(gzipPath))
        using (var gzip = new GZipStream(file, CompressionMode.Compress))
        {
            gzip.Write(tar, 0, secondPayloadOffset + second.Data.Length - 1);
        }

        return gzipPath;
    }

    private static byte[] BuildTar((string Name, byte[] Data)[] files)
    {
        using var stream = new MemoryStream();
        foreach (var (name, data) in files)
        {
            if (name.Length > 100)
            {
                // GNU long name entry
                var longHeader = new byte[512];
                WriteAscii(longHeader, 0, "././@LongLink");
                WriteOctal(longHeader, 124, 12, name.Length + 1);
                longHeader[156] = (byte)'L';
                WriteChecksum(longHeader);
                stream.Write(longHeader);
                var nameBytes = Encoding.UTF8.GetBytes(name + "\0");
                stream.Write(nameBytes);
                stream.Write(new byte[(512 - (nameBytes.Length % 512)) % 512]);
            }

            var header = new byte[512];
            WriteAscii(header, 0, name.Length > 100 ? name[..100] : name);
            WriteOctal(header, 124, 12, data.Length);
            header[156] = (byte)'0';
            WriteAscii(header, 257, "ustar\0");
            WriteAscii(header, 263, "00");
            WriteChecksum(header);
            stream.Write(header);
            stream.Write(data);
            stream.Write(new byte[(512 - (data.Length % 512)) % 512]);
        }

        stream.Write(new byte[1024]);
        return stream.ToArray();
    }

    private static void WriteAscii(byte[] buffer, int offset, string value) =>
        Encoding.ASCII.GetBytes(value, 0, Math.Min(value.Length, buffer.Length - offset), buffer, offset);

    private static void WriteOctal(byte[] buffer, int offset, int length, long value)
    {
        var octal = Convert.ToString(value, 8).PadLeft(length - 1, '0');
        Encoding.ASCII.GetBytes(octal + "\0", 0, length, buffer, offset);
    }

    private static void WriteChecksum(byte[] header)
    {
        Encoding.ASCII.GetBytes("        ", 0, 8, header, 148);
        var sum = header.Sum(value => value);
        var octal = Convert.ToString(sum, 8).PadLeft(6, '0') + "\0 ";
        Encoding.ASCII.GetBytes(octal, 0, 8, header, 148);
    }

    private sealed class SyncProgress<T>(Action<T> report) : IProgress<T>
    {
        public void Report(T value) => report(value);
    }
}
