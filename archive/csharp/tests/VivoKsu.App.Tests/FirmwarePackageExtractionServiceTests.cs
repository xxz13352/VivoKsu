using System.Collections;
using System.IO.Compression;
using VivoKsu.App.Models;
using VivoKsu.App.Services;

namespace VivoKsu.App.Tests;

public class FirmwarePackageExtractionServiceTests
{
    [Fact]
    public async Task ExtractAsync_copies_a_managed_image_to_a_staging_path()
    {
        var root = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        var packagePath = Path.Combine(root, "firmware.zip");
        Directory.CreateDirectory(root);

        try
        {
            using (var archive = ZipFile.Open(packagePath, ZipArchiveMode.Create))
            {
                WriteEntry(archive, "images/init_boot.img", "init-boot-payload");
                WriteEntry(archive, "images/super.img", "ignored");
            }

            var package = await new FirmwarePackageInspector().InspectAsync(packagePath, CancellationToken.None);
            var serviceType = typeof(FirmwarePackageInspector).Assembly.GetType("VivoKsu.App.Services.FirmwarePackageExtractionService");

            Assert.NotNull(serviceType);
            var service = Activator.CreateInstance(serviceType!);
            var method = serviceType.GetMethod("ExtractAsync");
            Assert.NotNull(method);

            var task = Assert.IsAssignableFrom<Task>(method!.Invoke(service, [package, "images/init_boot.img", CancellationToken.None]));
            await task;
            var result = task.GetType().GetProperty("Result")!.GetValue(task)!;
            var image = result.GetType().GetProperty("Image")!.GetValue(result)!;

            Assert.Equal(QuickFlashPartition.InitBoot, result.GetType().GetProperty("Partition")!.GetValue(result));
            Assert.True(File.Exists((string)image.GetType().GetProperty("Path")!.GetValue(image)!));
            Assert.Equal(17L, image.GetType().GetProperty("SizeBytes")!.GetValue(image));
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
