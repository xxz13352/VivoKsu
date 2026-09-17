using FluentAssertions;
using VivoKsu.App.Models;
using VivoKsu.App.Services;

namespace VivoKsu.App.Tests;

public class FastbootPartitionTableParserTests
{
    [Fact]
    public void Parse_returns_every_partition_size_and_marks_high_risk_rows_without_removing_them()
    {
        const string output = """
            (bootloader) current-slot: a
            (bootloader) partition-size:boot_a: 0x04000000
            (bootloader) partition-size:super: 0x200000000
            (bootloader) partition-type:boot_a: raw
            """;

        var snapshot = FastbootPartitionTableParser.Parse("FAST123", output);

        snapshot.Serial.Should().Be("FAST123");
        snapshot.Transport.Should().Be(PartitionTransportKind.Fastboot);
        snapshot.ActiveSlot.Should().Be("a");
        snapshot.Partitions.Select(partition => partition.Name).Should().Contain(["boot_a", "super"]);
        snapshot.Partitions.Single(partition => partition.Name == "boot_a").SizeBytes.Should().Be(64L * 1024 * 1024);
        snapshot.Partitions.Single(partition => partition.Name == "super").IsHighRisk.Should().BeTrue();
    }
}
