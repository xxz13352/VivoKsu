using VivoKsu.App.Models;
using VivoKsu.App.Services;

namespace VivoKsu.App.Tests;

public sealed class RootPatchArtifactServiceTests
{
    [Fact]
    public async Task ExportToDesktop_creates_a_folder_and_copies_each_patched_image()
    {
        var root = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        var sourceDirectory = Path.Combine(root, "source");
        var desktopDirectory = Path.Combine(root, "desktop");
        Directory.CreateDirectory(sourceDirectory);

        var initPath = Path.Combine(sourceDirectory, "payload_a.img");
        var vendorPath = Path.Combine(sourceDirectory, "payload_b.img");
        await File.WriteAllBytesAsync(initPath, "init-patched"u8.ToArray());
        await File.WriteAllBytesAsync(vendorPath, "vendor-patched"u8.ToArray());

        try
        {
            var service = new RootPatchArtifactService();
            var exported = service.ExportToDesktop(
            [
                new FlashImageInfo(initPath, new FileInfo(initPath).Length),
                new FlashImageInfo(vendorPath, new FileInfo(vendorPath).Length)
            ],
            desktopDirectory);

            var outputDirectory = Path.Combine(desktopDirectory, RootPatchArtifactService.OutputFolderName);
            Assert.True(Directory.Exists(outputDirectory));
            Assert.Equal(
                [
                    Path.Combine(outputDirectory, "payload_a.img"),
                    Path.Combine(outputDirectory, "payload_b.img")
                ],
                exported.Select(image => image.Path).ToArray());
            Assert.Equal("init-patched", await File.ReadAllTextAsync(exported[0].Path));
            Assert.Equal("vendor-patched", await File.ReadAllTextAsync(exported[1].Path));
        }
        finally
        {
            Directory.Delete(root, true);
        }
    }
}
