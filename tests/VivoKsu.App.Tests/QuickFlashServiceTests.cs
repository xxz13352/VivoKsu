using VivoKsu.App.Models;
using VivoKsu.App.Services;
using VivoKsu.App.ViewModels;

namespace VivoKsu.App.Tests;

public class QuickFlashServiceTests : IDisposable
{
    private readonly string imagePath = Path.Combine(Path.GetTempPath(), $"vivoksu-{Guid.NewGuid():N}.img");
    private readonly string secondImagePath = Path.Combine(Path.GetTempPath(), $"vivoksu-second-{Guid.NewGuid():N}.bin");

    [Fact]
    public async Task InspectImageAsync_returns_the_file_size_for_an_img_file()
    {
        var content = new byte[] { 0x56, 0x4B, 0x53, 0x55 };
        await File.WriteAllBytesAsync(imagePath, content);
        var service = CreateService();

        var image = await service.InspectImageAsync(imagePath, CancellationToken.None);

        Assert.Equal(imagePath, image.Path);
        Assert.Equal(4, image.SizeBytes);
    }

    [Fact]
    public async Task InspectImageAsync_accepts_a_bin_image_file()
    {
        var binPath = Path.ChangeExtension(imagePath, ".bin");
        await File.WriteAllBytesAsync(binPath, [0x56, 0x4B, 0x53, 0x55]);
        var service = CreateService();

        var image = await service.InspectImageAsync(binPath, CancellationToken.None);

        Assert.Equal(binPath, image.Path);
        Assert.Equal(4, image.SizeBytes);
        File.Delete(binPath);
    }

    [Fact]
    public async Task FlashAsync_flashes_an_approved_partition_then_reboots_a_matching_fastboot_device()
    {
        await File.WriteAllBytesAsync(imagePath, [0x01]);
        var fake = new FakeFastbootCliRunner();
        var logs = new OperationLogService();
        var service = CreateService(fake, logs);
        var session = new DeviceSessionViewModel();
        var image = await service.InspectImageAsync(imagePath, CancellationToken.None);

        await service.FlashAsync(session, QuickFlashPartition.InitBoot, image, FastbootTarget.Fastboot, CancellationToken.None);

        Assert.Equal(("init_boot", imagePath), fake.LastFlash);
        Assert.Contains("FAST123", fake.Rebooted);
        Assert.Equal(OperationKind.Completed, session.OperationKind);
        Assert.Contains(logs.Entries, entry => entry.Level == OperationLogLevel.Success);
    }

    [Fact]
    public async Task FlashImagesAsync_matches_a_fastbootd_device_when_target_is_fastbootd()
    {
        // VIVO 刷写目标为 fastbootd:仅当设备 getvar is-userspace=yes(userspace fastboot)
        // 时匹配,避免误连 bootloader 模式的 fastboot 设备。
        await File.WriteAllBytesAsync(imagePath, [0x01]);
        var fake = new FakeFastbootCliRunner
        {
            GetVarHandler = variable => variable == "is-userspace" ? "yes" : string.Empty
        };
        var service = CreateService(fake);
        var session = new DeviceSessionViewModel();

        await service.FlashImagesAsync(
            session,
            [new(QuickFlashPartition.Boot, new FlashImageInfo(imagePath, 1))],
            new(FastbootTarget.Fastbootd, true, false, false, true),
            CancellationToken.None);

        Assert.Equal([("FAST123", "boot", imagePath)], fake.FlashRequests);
        Assert.Contains("FAST123", fake.Rebooted);
        Assert.Equal(OperationKind.Completed, session.OperationKind);
    }

    [Fact]
    public async Task FlashAsync_marks_the_device_session_as_canceled_when_waiting_is_cancelled()
    {
        await File.WriteAllBytesAsync(imagePath, [0x01]);
        var logs = new OperationLogService();
        var service = CreateService(new FakeFastbootCliRunner(), logs);
        var session = new DeviceSessionViewModel();
        var image = await service.InspectImageAsync(imagePath, CancellationToken.None);
        using var cancellation = new CancellationTokenSource();
        cancellation.Cancel();

        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => service.FlashAsync(
            session,
            QuickFlashPartition.Boot,
            image,
            FastbootTarget.Fastboot,
            cancellation.Token));

        Assert.Equal(OperationKind.Canceled, session.OperationKind);
        Assert.Equal("快速刷写已取消", session.StatusText);
        Assert.False(session.IsBusy);
        Assert.Contains(logs.Entries, entry => entry.Level == OperationLogLevel.Warning && entry.Message.Contains("已取消"));
    }

    [Fact]
    public async Task FlashRootImagesAsync_flashes_all_images_in_order_and_reboots_once()
    {
        var vendorPath = Path.ChangeExtension(imagePath, ".bin");
        await File.WriteAllBytesAsync(imagePath, [0x01]);
        await File.WriteAllBytesAsync(vendorPath, [0x02]);
        var fake = new FakeFastbootCliRunner();
        var service = CreateService(fake, new OperationLogService());
        var session = new DeviceSessionViewModel();

        await service.FlashRootImagesAsync(
            session,
            [
                (QuickFlashPartition.InitBoot, new FlashImageInfo(imagePath, 1)),
                (QuickFlashPartition.VendorBoot, new FlashImageInfo(vendorPath, 1))
            ],
            FastbootTarget.Fastboot,
            CancellationToken.None);

        Assert.Equal(
            [("FAST123", "init_boot", imagePath), ("FAST123", "vendor_boot", vendorPath)],
            fake.FlashRequests);
        Assert.Contains("FAST123", fake.Rebooted);
        Assert.Equal(OperationKind.Completed, session.OperationKind);
        File.Delete(vendorPath);
    }

    [Fact]
    public async Task FlashImagesAsync_writes_each_selected_partition_to_a_then_b()
    {
        await File.WriteAllBytesAsync(imagePath, [0x01]);
        await File.WriteAllBytesAsync(secondImagePath, [0x02]);
        var fake = new FakeFastbootCliRunner
        {
            GetVarHandler = variable => variable.StartsWith("has-slot:", StringComparison.Ordinal) ? "yes" : "no"
        };
        var service = CreateService(fake);
        var session = new DeviceSessionViewModel();

        await service.FlashImagesAsync(
            session,
            [
                new(QuickFlashPartition.Boot, new FlashImageInfo(imagePath, 1)),
                new(QuickFlashPartition.InitBoot, new FlashImageInfo(secondImagePath, 1))
            ],
            new(FastbootTarget.Fastboot, true, true, false, false),
            CancellationToken.None);

        Assert.Equal(
        [
            ("FAST123", "boot_a", imagePath),
            ("FAST123", "boot_b", imagePath),
            ("FAST123", "init_boot_a", secondImagePath),
            ("FAST123", "init_boot_b", secondImagePath)
        ],
        fake.FlashRequests);
        Assert.Empty(fake.Rebooted);
    }

    [Fact]
    public async Task Dual_slot_preflight_rejects_an_unsupported_partition_before_any_write()
    {
        await File.WriteAllBytesAsync(imagePath, [0x01]);
        await File.WriteAllBytesAsync(secondImagePath, [0x02]);
        var fake = new FakeFastbootCliRunner
        {
            GetVarHandler = variable => variable == "has-slot:boot" ? "yes" : "no"
        };
        var service = CreateService(fake);

        await Assert.ThrowsAsync<InvalidOperationException>(() => service.FlashImagesAsync(
            new DeviceSessionViewModel(),
            [
                new(QuickFlashPartition.Boot, new FlashImageInfo(imagePath, 1)),
                new(QuickFlashPartition.VendorBoot, new FlashImageInfo(secondImagePath, 1))
            ],
            new(FastbootTarget.Fastboot, true, true, false, false),
            CancellationToken.None));

        Assert.Empty(fake.FlashRequests);
    }

    [Fact]
    public async Task Switch_slot_runs_after_all_flashes_and_before_optional_reboot()
    {
        await File.WriteAllBytesAsync(imagePath, [0x01]);
        var fake = new FakeFastbootCliRunner
        {
            GetVarHandler = variable => variable switch
            {
                "is-userspace" => "no",
                "current-slot" => "_a",
                _ when variable.StartsWith("has-slot:", StringComparison.Ordinal) => "yes",
                _ => string.Empty
            }
        };
        var service = CreateService(fake);

        await service.FlashImagesAsync(
            new DeviceSessionViewModel(),
            [new(QuickFlashPartition.Boot, new FlashImageInfo(imagePath, 1))],
            new(FastbootTarget.Fastboot, true, true, true, true),
            CancellationToken.None);

        Assert.Equal(["flash:boot_a", "flash:boot_b", "set-active:b", "reboot"], fake.Events);
    }

    [Fact]
    public async Task Flash_failure_prevents_slot_switch_and_reboot()
    {
        await File.WriteAllBytesAsync(imagePath, [0x01]);
        var fake = new FakeFastbootCliRunner
        {
            FailPartition = "boot_b",
            GetVarHandler = variable => variable switch
            {
                "is-userspace" => "no",
                "current-slot" => "a",
                _ when variable.StartsWith("has-slot:", StringComparison.Ordinal) => "yes",
                _ => string.Empty
            }
        };
        var service = CreateService(fake);

        await Assert.ThrowsAsync<InvalidOperationException>(() => service.FlashImagesAsync(
            new DeviceSessionViewModel(),
            [new(QuickFlashPartition.Boot, new FlashImageInfo(imagePath, 1))],
            new(FastbootTarget.Fastboot, true, true, true, true),
            CancellationToken.None));

        Assert.Empty(fake.SetActiveSlots);
        Assert.Empty(fake.Rebooted);
    }

    [Fact]
    public async Task Wait_disabled_checks_for_a_matching_device_once()
    {
        await File.WriteAllBytesAsync(imagePath, [0x01]);
        var native = new QuickFlashNativeApi { DeviceListing = string.Empty };
        var service = CreateService(new FakeFastbootCliRunner(), logs: new OperationLogService(), native);

        await Assert.ThrowsAsync<InvalidOperationException>(() => service.FlashImagesAsync(
            new DeviceSessionViewModel(),
            [new(QuickFlashPartition.Boot, new FlashImageInfo(imagePath, 1))],
            new(FastbootTarget.Fastboot, false, false, false, false),
            CancellationToken.None));

        Assert.Equal(1, native.DiscoveryCount);
    }

    private static QuickFlashService CreateService(
        FakeFastbootCliRunner? fake = null,
        OperationLogService? logs = null,
        QuickFlashNativeApi? native = null) =>
        new(
            new FastbootRsBackend(native ?? new QuickFlashNativeApi()),
            fake ?? new FakeFastbootCliRunner(),
            logs ?? new OperationLogService());

    public void Dispose()
    {
        File.Delete(imagePath);
        File.Delete(secondImagePath);
    }

    private sealed class QuickFlashNativeApi : IFastbootRsNativeApi
    {
        public string DeviceListing { get; init; } = "FAST123\tfastboot\n";
        public int DiscoveryCount { get; private set; }

        public string ListDevices()
        {
            DiscoveryCount++;
            return DeviceListing;
        }
        public string Shell(string? serial, string command, int timeoutMilliseconds = 15000) => string.Empty;
        public string GetVar(string? serial, string variable) => string.Empty;
        public void Reboot(string? serial, string target) { }
        public void Push(string? serial, string localPath, string remotePath) { }
        public long Pull(string? serial, string remotePath, string localPath) => 0;
        public string Install(string? serial, string apkPath, bool replace) => string.Empty;
        public void Flash(string? serial, string partition, string imagePath) { }
    }
}
