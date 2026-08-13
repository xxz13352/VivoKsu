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
            new Progress<VivoFirmwareExtractor.VivoProgress>(progress.Add),
            CancellationToken.None);

        results.Should().HaveCount(1);
        File.ReadAllBytes(Path.Combine(outputDirectory, "boot.img")).Should().Equal(new byte[] { 9, 8, 7, 6 });
        progress.Should().NotBeEmpty();
        progress.Last().Fraction.Should().BeGreaterThan(0);
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
}
