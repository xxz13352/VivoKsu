using System.Text;
using FluentAssertions;
using VivoKsu.App.Services;

namespace VivoKsu.App.Tests;

public class PayloadDumperRunnerTests
{
    private const int BlockSize = 4096;

    [Fact]
    public async Task ListPartitionsAsync_parses_partition_names_sizes_and_compression()
    {
        var runner = CreateRunner();
        if (!runner.IsAvailable)
        {
            return;
        }

        var payload = CreatePayloadFile();
        var partitions = await runner.ListPartitionsAsync(payload, CancellationToken.None);

        partitions.Select(partition => partition.Name).Should().BeEquivalentTo("boot", "init_boot", "vendor_boot");
        partitions.Single(partition => partition.Name == "boot").SizeBytes.Should().Be(2048);
        partitions.Single(partition => partition.Name == "init_boot").SizeBytes.Should().Be(1024);
        partitions.Should().OnlyContain(partition => partition.CompressionType == "none");
    }

    [Fact]
    public async Task ExtractAsync_produces_the_selected_partition_images()
    {
        var runner = CreateRunner();
        if (!runner.IsAvailable)
        {
            return;
        }

        var payload = CreatePayloadFile();
        var outputDirectory = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(outputDirectory);

        var results = await runner.ExtractAsync(payload, ["boot", "init_boot"], outputDirectory, CancellationToken.None);

        results.Select(result => result.PartitionName).Should().Contain("boot", "init_boot");
        File.ReadAllBytes(Path.Combine(outputDirectory, "boot.img")).Should().HaveCount(2048);
        File.ReadAllBytes(Path.Combine(outputDirectory, "init_boot.img")).Should().HaveCount(1024);
    }

    [Fact]
    public async Task ExtractAsync_reports_process_write_bytes_for_progress()
    {
        var runner = CreateRunner();
        if (!runner.IsAvailable)
        {
            return;
        }

        var payload = CreatePayloadFile();
        var outputDirectory = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        var reportedBytes = new List<long>();

        var results = await runner.ExtractAsync(
            payload, ["boot"], outputDirectory, CancellationToken.None, new SyncProgress<long>(reportedBytes.Add));

        results.Should().HaveCount(1);
        reportedBytes.Should().NotBeEmpty();
        reportedBytes.Should().BeInAscendingOrder();
    }

    private sealed class SyncProgress<T> : IProgress<T>
    {
        private readonly Action<T> handler;

        public SyncProgress(Action<T> handler) => this.handler = handler;

        public void Report(T value) => handler(value);
    }

    private static PayloadDumperRunner CreateRunner() => new(
        Path.Combine(AppContext.BaseDirectory, "payload-tools", "payload_dumper.exe"));

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

/// <summary>Builds a minimal synthetic Android OTA payload.bin (REPLACE ops, no compression).</summary>
internal static class PayloadTestData
{
    public static byte[] Build(IReadOnlyList<(string Name, byte[] Data)> partitions)
    {
        var manifest = new List<byte>();
        var offset = 0L;
        foreach (var (_, data) in partitions)
        {
            offset += data.Length;
        }

        offset = 0;
        foreach (var (name, data) in partitions)
        {
            manifest.AddRange(FieldLen(13, PartitionUpdate(name, data, offset)));
            offset += data.Length;
        }

        var blobs = partitions.SelectMany(partition => partition.Data).ToArray();
        var header = new List<byte>(Encoding.ASCII.GetBytes("CrAU"));
        header.AddRange(BigEndian(8, 2));      // format version
        header.AddRange(BigEndian(8, manifest.Count));
        header.AddRange(BigEndian(4, 0));      // metadata signature size
        header.AddRange(manifest);
        header.AddRange(blobs);
        return header.ToArray();
    }

    private static byte[] PartitionUpdate(string name, byte[] data, long dataOffset)
    {
        var info = FieldVarint(1, (ulong)data.Length);  // PartitionInfo.size
        var message = new List<byte>();
        message.AddRange(FieldStr(1, name));            // partition_name
        message.AddRange(FieldLen(7, info));            // new_partition_info
        message.AddRange(FieldLen(8, Operation(dataOffset, data.Length))); // operations
        return message.ToArray();
    }

    private static byte[] Operation(long dataOffset, int dataLength)
    {
        var numBlocks = Math.Max((dataLength + 4095) / 4096, 1);
        var extent = new List<byte>();
        extent.AddRange(FieldVarint(1, 0));             // Extent.start_block
        extent.AddRange(FieldVarint(2, (ulong)numBlocks)); // Extent.num_blocks

        var operation = new List<byte>();
        operation.AddRange(FieldVarint(1, 0));          // type = REPLACE
        operation.AddRange(FieldVarint(2, (ulong)dataOffset));
        operation.AddRange(FieldVarint(3, (ulong)dataLength));
        operation.AddRange(FieldLen(6, extent.ToArray())); // dst_extents
        return operation.ToArray();
    }

    private static byte[] BigEndian(int length, long value)
    {
        var buffer = new byte[length];
        for (var i = length - 1; i >= 0; i--)
        {
            buffer[i] = (byte)(value & 0xFF);
            value >>= 8;
        }

        return buffer;
    }

    private static byte[] Varint(ulong value)
    {
        var bytes = new List<byte>();
        while (true)
        {
            var current = (byte)(value & 0x7F);
            value >>= 7;
            if (value != 0)
            {
                bytes.Add((byte)(current | 0x80));
            }
            else
            {
                bytes.Add(current);
                return bytes.ToArray();
            }
        }
    }

    private static byte[] Tag(int field, int wireType) => Varint((ulong)((field << 3) | wireType));

    private static byte[] FieldVarint(int field, ulong value) =>
        Tag(field, 0).Concat(Varint(value)).ToArray();

    private static byte[] FieldLen(int field, byte[] data) =>
        Tag(field, 2).Concat(Varint((ulong)data.Length)).Concat(data).ToArray();

    private static byte[] FieldStr(int field, string value) => FieldLen(field, Encoding.UTF8.GetBytes(value));
}
