using System.IO;
using VivoKsu.App.Models;

namespace VivoKsu.App.Services;

public sealed class RootPatchArtifactService
{
    public const string OutputFolderName = "VivoKsu_修补镜像";

    private readonly Func<string> desktopDirectoryProvider;

    public RootPatchArtifactService(Func<string>? desktopDirectoryProvider = null)
    {
        this.desktopDirectoryProvider = desktopDirectoryProvider ?? (() =>
            Environment.GetFolderPath(Environment.SpecialFolder.DesktopDirectory));
    }

    public IReadOnlyList<FlashImageInfo> ExportToDesktop(
        IEnumerable<FlashImageInfo> images,
        string? desktopDirectory = null)
    {
        var selectedImages = images.ToArray();
        if (selectedImages.Length == 0)
        {
            return [];
        }

        var desktop = desktopDirectory ?? desktopDirectoryProvider();
        if (string.IsNullOrWhiteSpace(desktop))
        {
            throw new InvalidOperationException("未找到桌面目录，无法保存 ROOT 修补镜像。 ");
        }

        var outputDirectory = Path.Combine(desktop, OutputFolderName);
        Directory.CreateDirectory(outputDirectory);

        var exported = new List<FlashImageInfo>(selectedImages.Length);
        foreach (var image in selectedImages)
        {
            var fileName = Path.GetFileName(image.Path);
            if (string.IsNullOrWhiteSpace(fileName))
            {
                throw new InvalidDataException("ROOT 修补镜像缺少有效文件名。 ");
            }

            var destination = Path.GetFullPath(Path.Combine(outputDirectory, fileName));
            if (!string.Equals(Path.GetFullPath(image.Path), destination, StringComparison.OrdinalIgnoreCase))
            {
                File.Copy(image.Path, destination, overwrite: true);
            }

            var copied = new FileInfo(destination);
            if (!copied.Exists || copied.Length == 0)
            {
                throw new InvalidDataException($"ROOT 修补镜像保存失败: {destination}");
            }

            exported.Add(new FlashImageInfo(copied.FullName, copied.Length));
        }

        return exported;
    }
}
