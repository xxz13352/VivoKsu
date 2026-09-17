using FluentAssertions;
using VivoKsu.App.Models;
using VivoKsu.App.Services;

namespace VivoKsu.App.Tests;

public class PartitionExecutionPlanBuilderTests
{
    [Fact]
    public void BuildWrite_keeps_the_selected_partition_and_any_img_filename()
    {
        var partition = new DevicePartition(
            "boot_a",
            "boot_a",
            64L * 1024 * 1024,
            "a",
            false,
            false,
            false);

        var plan = new PartitionExecutionPlanBuilder().BuildWrite(
            "FAST123",
            PartitionTransportKind.Fastboot,
            [partition],
            new Dictionary<string, string> { ["boot_a"] = @"D:\images\custom.bin" });

        plan.Serial.Should().Be("FAST123");
        plan.Transport.Should().Be(PartitionTransportKind.Fastboot);
        plan.Operation.Should().Be(PartitionOperationKind.Write);
        plan.Tasks.Should().ContainSingle();
        plan.Tasks[0].PartitionName.Should().Be("boot_a");
        plan.Tasks[0].ImagePath.Should().Be(@"D:\images\custom.bin");
    }

    [Fact]
    public void BuildWrite_rejects_a_missing_image_for_adb_root()
    {
        var partition = CreateAdbPartition(64);
        var missingImagePath = Path.Combine(Path.GetTempPath(), $"missing-{Guid.NewGuid():N}.img");

        var action = () => new PartitionExecutionPlanBuilder().BuildWrite(
            "ADB123",
            PartitionTransportKind.AdbRoot,
            [partition],
            new Dictionary<string, string> { [partition.Name] = missingImagePath });

        action.Should().Throw<InvalidOperationException>()
            .WithMessage("*不存在*");
    }

    [Fact]
    public void BuildWrite_rejects_an_empty_image_for_adb_root()
    {
        var imagePath = CreateTemporaryImage([]);
        try
        {
            var action = () => new PartitionExecutionPlanBuilder().BuildWrite(
                "ADB123",
                PartitionTransportKind.AdbRoot,
                [CreateAdbPartition(64)],
                new Dictionary<string, string> { ["boot_a"] = imagePath });

            action.Should().Throw<InvalidOperationException>()
                .WithMessage("*为空*");
        }
        finally
        {
            File.Delete(imagePath);
        }
    }

    [Fact]
    public void BuildWrite_rejects_an_image_larger_than_a_known_adb_root_partition()
    {
        var imagePath = CreateTemporaryImage([1, 2, 3, 4, 5]);
        try
        {
            var action = () => new PartitionExecutionPlanBuilder().BuildWrite(
                "ADB123",
                PartitionTransportKind.AdbRoot,
                [CreateAdbPartition(4)],
                new Dictionary<string, string> { ["boot_a"] = imagePath });

            action.Should().Throw<InvalidOperationException>()
                .WithMessage("*大于分区*");
        }
        finally
        {
            File.Delete(imagePath);
        }
    }

    [Fact]
    public void BuildWrite_rejects_an_android_sparse_image_for_adb_root()
    {
        var imagePath = CreateTemporaryImage([0x3A, 0xFF, 0x26, 0xED]);
        try
        {
            var action = () => new PartitionExecutionPlanBuilder().BuildWrite(
                "ADB123",
                PartitionTransportKind.AdbRoot,
                [CreateAdbPartition(64)],
                new Dictionary<string, string> { ["boot_a"] = imagePath });

            action.Should().Throw<InvalidOperationException>()
                .WithMessage("*sparse*");
        }
        finally
        {
            File.Delete(imagePath);
        }
    }

    [Fact]
    public void BuildErase_keeps_mounted_and_high_risk_partitions_in_the_plan()
    {
        var partition = new DevicePartition(
            "super",
            "/dev/block/sda70",
            8L * 1024 * 1024 * 1024,
            string.Empty,
            true,
            true,
            true);

        var plan = new PartitionExecutionPlanBuilder().BuildErase(
            "ADB123",
            PartitionTransportKind.AdbRoot,
            [partition]);

        plan.Tasks.Should().ContainSingle();
        plan.Tasks[0].PartitionName.Should().Be("super");
        plan.Tasks[0].DevicePath.Should().Be("/dev/block/sda70");
        plan.Tasks[0].ImagePath.Should().BeNull();
    }

    [Fact]
    public void BuildBackup_assigns_an_img_output_under_the_selected_directory()
    {
        var partition = new DevicePartition(
            "vendor_boot_b",
            "/dev/block/sda22",
            96L * 1024 * 1024,
            "b",
            false,
            false,
            true);

        var plan = new PartitionExecutionPlanBuilder().BuildBackup(
            "ADB123",
            PartitionTransportKind.AdbRoot,
            [partition],
            @"D:\backups");

        plan.Tasks.Should().ContainSingle();
        plan.Tasks[0].OutputPath.Should().Be(@"D:\backups\vendor_boot_b.img");
    }

    private static DevicePartition CreateAdbPartition(long sizeBytes) => new(
        "boot_a",
        "/dev/block/by-name/boot_a",
        sizeBytes,
        "a",
        false,
        false,
        false);

    private static string CreateTemporaryImage(byte[] contents)
    {
        var path = Path.Combine(Path.GetTempPath(), $"partition-plan-{Guid.NewGuid():N}.img");
        File.WriteAllBytes(path, contents);
        return path;
    }
}
