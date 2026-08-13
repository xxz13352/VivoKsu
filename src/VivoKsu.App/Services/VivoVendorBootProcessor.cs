using System.IO;
using System.Text.RegularExpressions;
using VivoKsu.App.Models;

namespace VivoKsu.App.Services;

public sealed class VivoVendorBootProcessor
{
    private const long MaxPatchGrowthBytes = 16L * 1024 * 1024;
    private const string RemoteBase = "/data/local/tmp/vivo_vendor_boot";
    private static readonly Regex GkiDirectoryPattern = new(
        @"lib/modules/(?<version>[0-9][^/\r\n]*?)-gki(?:/|\r?$)",
        RegexOptions.Compiled | RegexOptions.CultureInvariant | RegexOptions.Multiline);

    // The captured module directory name is device-controlled and is later spliced
    // into adb shell commands, so it must match a strict allowlist.
    private static readonly Regex GkiTargetDirectoryRegex = new(
        @"^lib/modules/[0-9][A-Za-z0-9._+-]*-gki$",
        RegexOptions.Compiled | RegexOptions.CultureInvariant);

    private readonly FastbootRsBackend backend;
    private readonly VivoRootResourceService resources;
    private readonly QuickFlashService imageInspector;

    public VivoVendorBootProcessor(
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
        FlashImageInfo source,
        CancellationToken cancellationToken,
        OperationContext? context = null)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(serial);
        context?.ReportStage("正在准备 vendor_boot 修补资源");
        var sourceSnapshot = await InspectSourceAsync(source, cancellationToken);
        var tool = resources.ResolveMagiskboot();
        var remoteRoot = $"{RemoteBase}_{Guid.NewGuid():N}";
        var localOutput = Path.Combine(
            Path.GetDirectoryName(source.Path) ?? Path.GetTempPath(),
            $"{Path.GetFileNameWithoutExtension(source.Path)}_vivo_patched.img");

        if (File.Exists(localOutput))
        {
            throw new InvalidOperationException($"vendor_boot 修补输出已存在: {localOutput}");
        }

        try
        {
            await backend.ShellAsync(serial, $"mkdir -p {remoteRoot}", cancellationToken);
            context?.ReportStage("正在上传 vendor_boot 修补工具");
            await backend.PushAsync(serial, tool.Path, $"{remoteRoot}/magiskboot", cancellationToken);
            context?.ReportStage("正在上传 vendor_boot 镜像");
            await backend.PushAsync(serial, sourceSnapshot.Path, $"{remoteRoot}/vendor_boot.img", cancellationToken);

            context?.ReportStage("正在处理 vendor_boot 镜像");
            var unpack = await backend.ShellAsync(
                serial,
                $"cd {remoteRoot} && chmod 755 magiskboot && ./magiskboot unpack vendor_boot.img " +
                ">/dev/null 2>&1 && test -f vendor_boot/vendor_ramdisk/ramdisk.cpio && echo VENDOR_RAMDISK_READY",
                cancellationToken);
            EnsureOutput(unpack, "VENDOR_RAMDISK_READY", "vendor_boot 未找到可处理的 vendor_ramdisk/ramdisk.cpio");

            var listing = await backend.ShellAsync(
                serial,
                $"cd {remoteRoot}/vendor_boot/vendor_ramdisk && ../../magiskboot cpio ramdisk.cpio \"ls /lib/modules/\"",
                cancellationToken);
            // 官版 KernelSU 一次处理全部内核路径：官方内核( lib/modules )加上 vendor
            // ramdisk 里检测到的所有 GKI 模块目录，避免漏掉任一模块加载清单。
            var targetDirectories = ResolveTargetDirectories(listing);

            foreach (var targetDirectory in targetDirectories)
            {
                // 官方内核的模块清单是 vendor_boot 的必需部分；GKI 目录缺失时跳过、
                // 不中断修补(缺 GKI 也照常处理官方内核)。
                var requireFile = targetDirectory == "lib/modules";
                foreach (var fileName in new[] { "modules.load", "modules.load.recovery", "modules.softdep" })
                {
                    var path = $"{targetDirectory}/{fileName}";
                    var extract = await backend.ShellAsync(
                        serial,
                        $"cd {remoteRoot}/vendor_boot/vendor_ramdisk && ../../magiskboot cpio ramdisk.cpio \"extract {path} {fileName}\" " +
                        $"&& test -f {fileName} && echo READY",
                        cancellationToken);
                    if (!extract.Contains("READY", StringComparison.OrdinalIgnoreCase))
                    {
                        if (!requireFile)
                        {
                            continue;
                        }

                        throw new InvalidDataException($"vendor_boot 缺少 {path}");
                    }

                    var filter = fileName == "modules.softdep"
                        ? @"sed -i '/softdep[[:space:]]\+vr[[:space:]]\+pre/d' modules.softdep"
                        : $"sed -i '/vr\\.ko/d' {fileName}";
                    await backend.ShellAsync(
                        serial,
                        $"cd {remoteRoot}/vendor_boot/vendor_ramdisk && {filter} && " +
                        $"../../magiskboot cpio ramdisk.cpio \"add 0644 {path} {fileName}\" && rm -f {fileName}",
                        cancellationToken);
                }
            }

            var repack = await backend.ShellAsync(
                serial,
                $"cd {remoteRoot}/vendor_boot && ../magiskboot repack vendor_boot.img " +
                ">/dev/null 2>&1 && test -f new-boot.img && echo REPACKED",
                cancellationToken);
            EnsureOutput(repack, "REPACKED", "vendor_boot 重打包失败，未生成 new-boot.img");

            context?.ReportStage("正在获取修补后的 vendor_boot 镜像");
            var pulledLength = await backend.PullAsync(serial, $"{remoteRoot}/vendor_boot/new-boot.img", localOutput, cancellationToken);
            if (pulledLength <= 0 || !File.Exists(localOutput))
            {
                throw new InvalidDataException("未能获取有效的 vendor_boot 修补镜像。" );
            }

            var patched = await imageInspector.InspectImageAsync(localOutput, cancellationToken);
            if (patched.SizeBytes != pulledLength || patched.SizeBytes > sourceSnapshot.SizeBytes + MaxPatchGrowthBytes)
            {
                throw new InvalidDataException("vendor_boot 修补镜像大小异常。" );
            }

            return patched;
        }
        finally
        {
            try
            {
                await backend.ShellAsync(serial, $"rm -rf {remoteRoot}", CancellationToken.None);
            }
            catch
            {
                // Preserve the primary processing error; remote cleanup is best effort.
            }
        }
    }

    private static IReadOnlyList<string> ResolveTargetDirectories(string listing)
    {
        var targets = new List<string> { "lib/modules" };
        foreach (Match match in GkiDirectoryPattern.Matches(listing))
        {
            var target = $"lib/modules/{match.Groups["version"].Value}-gki";
            if (GkiTargetDirectoryRegex.IsMatch(target))
            {
                targets.Add(target);
            }
        }

        return targets;
    }

    private async Task<FlashImageInfo> InspectSourceAsync(FlashImageInfo source, CancellationToken cancellationToken)
    {
        return await imageInspector.InspectImageAsync(source.Path, cancellationToken);
    }

    private static void EnsureOutput(string output, string marker, string message)
    {
        if (!output.Contains(marker, StringComparison.OrdinalIgnoreCase))
        {
            throw new InvalidDataException(message);
        }
    }
}
