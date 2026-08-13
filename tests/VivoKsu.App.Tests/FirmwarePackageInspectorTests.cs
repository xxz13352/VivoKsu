using System.IO.Compression;
using VivoKsu.App.Services;

namespace VivoKsu.App.Tests;

public class FirmwarePackageInspectorTests
{
    [Fact]
    public void ManagedImageEntries_keeps_only_supported_flash_partitions()
    {
        var inspection = new VivoKsu.App.Models.FirmwarePackageInspection(
            "C:\\firmware.zip",
            "firmware.zip",
            5,
            ["images/boot.img", "images/init_boot.img", "images/vendor_boot.img", "images/lk.img", "images/super.img"]);
        var managedImages = typeof(VivoKsu.App.Models.FirmwarePackageInspection)
            .GetProperty("ManagedImageEntries")?
            .GetValue(inspection) as IReadOnlyList<string>;

        Assert.NotNull(managedImages);
        Assert.Equal(["images/boot.img", "images/init_boot.img", "images/vendor_boot.img", "images/lk.img"], managedImages);
    }

    [Fact]
    public async Task InspectAsync_lists_image_entries_and_ignores_non_image_files()
    {
        var root = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        var packagePath = Path.Combine(root, "PD2307_A14.zip");
        Directory.CreateDirectory(root);

        try
        {
            using (var archive = ZipFile.Open(packagePath, ZipArchiveMode.Create))
            {
                WriteEntry(archive, "META-INF/com/android/metadata", "device=vivo");
                WriteEntry(archive, "images/vendor_boot.img", "vendor");
                WriteEntry(archive, "images/boot.img", "boot");
                WriteEntry(archive, "payload.bin", "payload");
            }

            var result = await new FirmwarePackageInspector().InspectAsync(packagePath, CancellationToken.None);

            Assert.Equal("PD2307_A14.zip", result.PackageName);
            Assert.Equal(4, result.EntryCount);
            Assert.Equal(["images/boot.img", "images/vendor_boot.img"], result.ImageEntries);
        }
        finally
        {
            Directory.Delete(root, true);
        }
    }

    private static void WriteEntry(ZipArchive archive, string name, string contents)
    {
        using var writer = new StreamWriter(archive.CreateEntry(name).Open());
        writer.Write(contents);
    }
}
