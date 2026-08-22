using System.IO;
using VivoKsu.App.Models;

namespace VivoKsu.App.Services;

public sealed class VivoKsuDevicePatchService
{
    private const long MaxPatchGrowthBytes = 16L * 1024 * 1024;
    private const string RemoteDirectory = "/data/local/tmp";
    private const string RemoteLibrary = RemoteDirectory + "/vivoksu_libksud.so";
    private const string RemoteSource = RemoteDirectory + "/vivoksu_init_boot.img";
    private const string RemotePatched = RemoteDirectory + "/vivoksu_patched_init_boot.img";

    private readonly FastbootRsBackend backend;
    private readonly VivoRootResourceService resources;
    private readonly QuickFlashService imageInspector;

    public VivoKsuDevicePatchService(
        FastbootRsBackend backend,
        VivoRootResourceService resources,
        QuickFlashService imageInspector)
    {
        this.backend = backend;
        this.resources = resources;
        this.imageInspector = imageInspector;
    }

    public async Task<FlashImageInfo> PatchAsync(
        string serial,
        string managerKey,
        string kmi,
        FlashImageInfo source,
        CancellationToken cancellationToken,
        OperationContext? context = null)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(serial);
        VivoRootResourceService.ValidateKmi(kmi);
        context?.ReportStage("正在准备 ROOT 修补资源");
        var sourceSnapshot = await InspectSourceAsync(source, cancellationToken);
        var manager = resources.ResolveManager(managerKey);
        var workspace = Path.Combine(Path.GetTempPath(), "VivoKsu", "root", Guid.NewGuid().ToString("N"));
        var libraryPath = Path.Combine(workspace, "libksud.so");
        var stagedOutput = Path.Combine(workspace, "patched_init_boot.img");
        var outputPath = Path.Combine(
            Path.GetDirectoryName(source.Path) ?? workspace,
            $"{Path.GetFileNameWithoutExtension(source.Path)}_vivoksu_patched.img");

        Directory.CreateDirectory(workspace);
        try
        {
            resources.ExtractVerifiedLibKsud(manager, "arm64-v8a", libraryPath);
            context?.ReportStage("正在上传 ROOT 修补工具");
            await backend.PushAsync(serial, libraryPath, RemoteLibrary, cancellationToken);
            context?.ReportStage("正在上传 init_boot 镜像");
            await backend.PushAsync(serial, sourceSnapshot.Path, RemoteSource, cancellationToken);
            var patchCommand =
                $"cd {RemoteDirectory} && chmod 755 {Path.GetFileName(RemoteLibrary)} && " +
                $"TMPDIR={RemoteDirectory} ./{Path.GetFileName(RemoteLibrary)} boot-patch " +
                $"-b {Path.GetFileName(RemoteSource)} --out {RemoteDirectory} " +
                $"--out-name {Path.GetFileName(RemotePatched)} --partition init_boot --kmi {kmi}";
            context?.ReportStage("正在处理 init_boot 镜像");
            await backend.ShellAsync(serial, patchCommand, cancellationToken);
            context?.ReportStage("正在获取修补后的 init_boot 镜像");
            var pulledLength = await backend.PullAsync(serial, RemotePatched, stagedOutput, cancellationToken);
            if (pulledLength <= 0 || !File.Exists(stagedOutput))
            {
                throw new InvalidDataException("未能从设备获取有效的 KernelSU 修补镜像。");
            }

            var patched = await imageInspector.InspectImageAsync(stagedOutput, cancellationToken);
            if (patched.SizeBytes != pulledLength || patched.SizeBytes > sourceSnapshot.SizeBytes + MaxPatchGrowthBytes)
            {
                throw new InvalidDataException("KernelSU 修补镜像大小异常。");
            }

            // Validation passed; only now publish to the final path, overwriting any
            // stale partial left by an interrupted earlier attempt.
            File.Move(stagedOutput, outputPath, overwrite: true);
            return patched with { Path = outputPath };
        }
        finally
        {
            try
            {
                await backend.ShellAsync(serial, $"rm -f {RemoteLibrary} {RemoteSource} {RemotePatched}", CancellationToken.None);
            }
            catch
            {
                // The root flow has already produced its primary error; cleanup is best effort.
            }

            Directory.Delete(workspace, recursive: true);
        }
    }

    private async Task<FlashImageInfo> InspectSourceAsync(FlashImageInfo source, CancellationToken cancellationToken)
    {
        var current = await imageInspector.InspectImageAsync(source.Path, cancellationToken);
        return current;
    }
}
