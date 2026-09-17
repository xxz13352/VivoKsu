using System.IO;
using System.Net.Http;
using VivoKsu.App.Models;

namespace VivoKsu.App.Services;

/// <summary>ROOT 云端 OTA 提取结果(对齐 Rust <c>RootOtaExtractedImages</c>)。</summary>
/// <param name="BootImage">选中的启动槽位镜像(init_boot 优先,否则 boot;ZIP 里没有时为 null)。</param>
/// <param name="BootPartitionName">实际选中的启动槽位分区名;无启动镜像时为空串。</param>
/// <param name="VendorBoot">vendor_boot 镜像(ZIP 里可能缺失)。</param>
/// <param name="StagingDirectory">提取暂存目录(调用方负责清理策略)。</param>
public sealed record RootOtaExtractedImages(
    FlashImageInfo? BootImage,
    string BootPartitionName,
    FlashImageInfo? VendorBoot,
    string StagingDirectory);

/// <summary>
/// ROOT 云端 OTA 提取服务(对齐 Rust <c>root_ota.rs</c> + <c>device_identity</c>):
/// 读设备 PD/版本 → <c>/api/rom</c> 解析 OTA 链接 → 对远程 ZIP 用 Range 请求按需流式提取
/// 修补所需的启动分区镜像,**不下载整包**。
/// OTA 链接只存在于本服务内存中,不写入日志/不进入 UI(对齐 Rust「URL 不进浏览器」)。
/// </summary>
public sealed class RootOtaCloudExtractService
{
    /// <summary>启动槽位候选:init_boot 优先(Android 13+ 设备),回退 boot;vendor_boot 独立提取。</summary>
    private static readonly string[] BootPartitionCandidates = ["init_boot", "boot"];

    private const string VendorBootPartition = "vendor_boot";

    private readonly OtaApiClient otaClient;
    private readonly HttpClient http;
    private readonly FastbootRsBackend? backend;

    public RootOtaCloudExtractService(OtaApiClient otaClient, FastbootRsBackend? backend, HttpClient? http = null)
    {
        this.otaClient = otaClient;
        this.backend = backend;
        this.http = http ?? new HttpClient(ApiTlsPinPolicy.CreateHandler()) { Timeout = TimeSpan.FromSeconds(120) };
    }

    /// <summary>
    /// 检查云端 OTA 是否可用(设备 PD/版本是否能在服务端解析到 ROM)。
    /// 返回是否可用;失败原因写入 <paramref name="unavailableReason"/>(不包含 OTA URL)。
    /// </summary>
    public async Task<bool> CheckAvailableAsync(
        string pd, string version, Action<string>? reportStage = null, CancellationToken cancellationToken = default)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(pd);
        ArgumentException.ThrowIfNullOrWhiteSpace(version);
        reportStage?.Invoke("正在查询固件服务端");
        try
        {
            var rom = await otaClient.ResolveAsync(pd, version, cancellationToken).ConfigureAwait(false);
            // 只暴露格式与大小,URL 留在服务内存。
            reportStage?.Invoke(string.IsNullOrWhiteSpace(rom.Name)
                ? $"服务端已解析到 ROM({FormatSize(rom.SizeBytes)})"
                : $"服务端已解析到 ROM: {rom.Name}({FormatSize(rom.SizeBytes)})");
            return !string.IsNullOrWhiteSpace(rom.Url);
        }
        catch (Exception exception) when (exception is not OperationCanceledException)
        {
            reportStage?.Invoke($"服务端解析失败: {exception.Message}");
            return false;
        }
    }

    /// <summary>
    /// 云提取修补所需的启动分区镜像(init_boot/boot + vendor_boot)。
    /// 产物写入 <paramref name="stagingRoot"/> 下的独立目录,提取完成即返回本地 <see cref="FlashImageInfo"/>,
    /// 后续修补/刷写流程与手选镜像完全一致。
    /// </summary>
    public async Task<RootOtaExtractedImages> ExtractAsync(
        string pd,
        string version,
        string stagingRoot,
        Action<string>? reportStage = null,
        CancellationToken cancellationToken = default)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(pd);
        ArgumentException.ThrowIfNullOrWhiteSpace(version);
        ArgumentException.ThrowIfNullOrWhiteSpace(stagingRoot);

        reportStage?.Invoke("正在查询固件服务端");
        var rom = await otaClient.ResolveAsync(pd, version, cancellationToken).ConfigureAwait(false);
        if (string.IsNullOrWhiteSpace(rom.Url))
        {
            throw new InvalidOperationException("服务端未返回可用的固件链接。");
        }

        var staging = Path.Combine(stagingRoot, $"root-ota-{DateTimeOffset.UtcNow.ToUnixTimeSeconds()}");
        Directory.CreateDirectory(staging);

        reportStage?.Invoke("正在云端提取启动分区(只下载所需字节)");
        var wanted = BootPartitionCandidates.Append(VendorBootPartition).ToList();
        var extracted = await RemoteZipClient.ExtractMembersAsync(
            http, rom.Url, wanted, staging, (_, _) => { }, cancellationToken).ConfigureAwait(false);

        var boot = extracted.FirstOrDefault(image => image.PartitionName == "init_boot")
            ?? extracted.FirstOrDefault(image => image.PartitionName == "boot");
        var vendor = extracted.FirstOrDefault(image => image.PartitionName == VendorBootPartition);

        if (boot is null)
        {
            var names = string.Join(", ", extracted.Select(image => image.PartitionName));
            throw new InvalidOperationException(string.IsNullOrEmpty(names)
                ? "固件包内未找到 init_boot / boot 镜像。"
                : $"固件包内未找到 init_boot / boot 镜像(仅含: {names})。");
        }

        reportStage?.Invoke(
            vendor is null
                ? $"云端提取完成: {boot.PartitionName}"
                : $"云端提取完成: {boot.PartitionName} + {vendor.PartitionName}");

        return new RootOtaExtractedImages(
            new FlashImageInfo(boot.Path, boot.SizeBytes),
            boot.PartitionName,
            vendor is null ? null : new FlashImageInfo(vendor.Path, vendor.SizeBytes),
            staging);
    }

    /// <summary>
    /// 读取设备版本号(优先 <c>ro.build.version.bbk</c> 权威串,回退候选属性)。
    /// 返回 (version, codename);无 ADB 后端(测试)或读取失败返回空串。
    /// </summary>
    public async Task<(string Version, string Codename)> ReadDeviceVersionAsync(string serial, CancellationToken cancellationToken = default)
    {
        if (backend is null || string.IsNullOrWhiteSpace(serial))
        {
            return (string.Empty, string.Empty);
        }

        try
        {
            var bbk = (await backend.ShellAsync(serial, "getprop ro.build.version.bbk", cancellationToken).ConfigureAwait(false)).Trim();
            if (!string.IsNullOrWhiteSpace(bbk))
            {
                var (codename, version) = VivoVersionParser.ParseBbkVersion(bbk);
                if (!string.IsNullOrWhiteSpace(version) && !VivoVersionParser.IsGenericVersion(version))
                {
                    return (version, codename);
                }
            }
        }
        catch
        {
            // 回退到候选属性。
        }

        foreach (var prop in VivoVersionParser.VersionCandidates)
        {
            try
            {
                var value = (await backend.ShellAsync(serial, $"getprop {prop}", cancellationToken).ConfigureAwait(false)).Trim();
                if (!string.IsNullOrWhiteSpace(value) && !VivoVersionParser.IsGenericVersion(value))
                {
                    return (value, string.Empty);
                }
            }
            catch
            {
                // 尝试下一个候选属性。
            }
        }

        return (string.Empty, string.Empty);
    }

    private static string FormatSize(long? sizeBytes) => sizeBytes is not > 0
        ? "大小未知"
        : sizeBytes >= 1024 * 1024 * 1024
            ? $"{sizeBytes.Value / 1024.0 / 1024 / 1024:F2} GB"
            : $"{sizeBytes.Value / 1024.0 / 1024:F1} MB";
}
