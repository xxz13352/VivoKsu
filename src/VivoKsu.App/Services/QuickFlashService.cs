using System.IO;
using VivoKsu.App.Models;
using VivoKsu.App.ViewModels;

namespace VivoKsu.App.Services;

public sealed class QuickFlashService
{
    private readonly FastbootRsBackend backend;
    private readonly IFastbootCliRunner cliRunner;
    private readonly OperationLogService logs;

    public QuickFlashService(FastbootRsBackend backend, IFastbootCliRunner cliRunner, OperationLogService logs)
    {
        this.backend = backend;
        this.cliRunner = cliRunner;
        this.logs = logs;
    }

    public async Task<FlashImageInfo> InspectImageAsync(string imagePath, CancellationToken cancellationToken)
    {
        var extension = Path.GetExtension(imagePath);
        if (!string.Equals(extension, ".img", StringComparison.OrdinalIgnoreCase) &&
            !string.Equals(extension, ".bin", StringComparison.OrdinalIgnoreCase))
        {
            throw new ArgumentException("快速刷写仅接受 .img 或 .bin 镜像文件。", nameof(imagePath));
        }

        var file = new FileInfo(imagePath);
        if (!file.Exists)
        {
            throw new FileNotFoundException("未找到镜像文件。", imagePath);
        }

        if (file.Length == 0)
        {
            throw new InvalidDataException("镜像文件为空，无法刷写。");
        }

        return new FlashImageInfo(file.FullName, file.Length);
    }

    public async Task FlashAsync(
        DeviceSessionViewModel session,
        QuickFlashPartition partition,
        FlashImageInfo image,
        FastbootTarget target,
        CancellationToken cancellationToken,
        OperationContext? context = null)
    {
        await FlashImagesAsync(
            session,
            [new QuickFlashRequest(partition, image)],
            new QuickFlashOptions(
                target,
                WaitForDevice: true,
                FlashBothSlots: false,
                SwitchSlotAfterFlash: false,
                AutoReboot: true),
            cancellationToken,
            context);
    }

    public async Task FlashImagesAsync(
        DeviceSessionViewModel session,
        IReadOnlyList<QuickFlashRequest> requests,
        QuickFlashOptions options,
        CancellationToken cancellationToken,
        OperationContext? context = null,
        IProgress<(string Partition, double Fraction)>? flashProgress = null)
    {
        if (requests.Count == 0)
        {
            throw new ArgumentException("快速刷写至少需要选择一个镜像。", nameof(requests));
        }

        var partitionLabel = requests.Count == 1
            ? ToPartitionName(requests[0].Partition)
            : "预设分区";
        ReportWaiting(session, options.Target, partitionLabel, context);

        try
        {
            var device = await ResolveTargetAsync(options.Target, options.WaitForDevice, cancellationToken);
            session.ApplyDevice(device);

            string? nextSlot = null;
            if (options.FlashBothSlots)
            {
                foreach (var request in requests)
                {
                    var partitionName = ToPartitionName(request.Partition);
                    var hasSlot = await cliRunner.GetVarAsync(device.Serial, $"has-slot:{partitionName}", cancellationToken);
                    if (!IsTrueFastbootValue(hasSlot))
                    {
                        throw new InvalidOperationException($"设备分区 {partitionName} 不支持 A/B 双槽刷写。");
                    }
                }

                if (options.SwitchSlotAfterFlash)
                {
                    var currentSlot = await cliRunner.GetVarAsync(device.Serial, "current-slot", cancellationToken);
                    nextSlot = NormalizeCurrentSlot(currentSlot) == "a" ? "b" : "a";
                }
            }

            foreach (var request in requests)
            {
                var partitionName = ToPartitionName(request.Partition);
                var targetPartitions = options.FlashBothSlots
                    ? new[] { $"{partitionName}_a", $"{partitionName}_b" }
                    : new[] { partitionName };

                foreach (var targetPartition in targetPartitions)
                {
                    ReportFlashing(session, targetPartition, request.Image, context);
                    await cliRunner.FlashAsync(
                        device.Serial,
                        targetPartition,
                        request.Image.Path,
                        CreateFlashProgress(context, flashProgress, targetPartition),
                        cancellationToken);
                }
            }

            if (nextSlot is not null)
            {
                if (context is null)
                {
                    session.BeginOperation(OperationKind.Flashing, $"刷写完成，正在切换到槽位 {nextSlot}");
                    logs.Write(OperationLogLevel.Info, $"全部分区刷写成功，正在切换到槽位 {nextSlot}。");
                }
                else
                {
                    context.ReportStage($"刷写完成，正在切换到槽位 {nextSlot}");
                }

                await cliRunner.SetActiveAsync(device.Serial, nextSlot, cancellationToken);
            }

            if (options.AutoReboot)
            {
                if (context is null)
                {
                    session.BeginOperation(OperationKind.Rebooting, "刷写完成，正在重启设备");
                }
                else
                {
                    context.ReportStage("刷写完成，正在重启设备");
                }

                await cliRunner.RebootAsync(device.Serial, cancellationToken);
            }

            if (context is null)
            {
                var completion = options.AutoReboot
                    ? $"{partitionLabel} 已刷写，设备正在重启"
                    : $"{partitionLabel} 已刷写完成";
                session.CompleteOperation(completion);
                logs.Write(
                    OperationLogLevel.Success,
                    options.AutoReboot
                        ? $"{partitionLabel} 刷写完成，已发送普通重启指令。"
                        : $"{partitionLabel} 刷写完成。");
            }
        }
        catch (OperationCanceledException)
        {
            if (context is null)
            {
                session.CancelOperation("快速刷写已取消");
                logs.Write(OperationLogLevel.Warning, "快速刷写已取消。");
            }

            throw;
        }
        catch (Exception exception)
        {
            if (context is null)
            {
                session.FailOperation("快速刷写失败");
                logs.Write(OperationLogLevel.Error, exception.Message);
            }

            throw;
        }
    }

    public async Task FlashRootImagesAsync(
        DeviceSessionViewModel session,
        IReadOnlyList<(QuickFlashPartition Partition, FlashImageInfo Image)> images,
        FastbootTarget target,
        CancellationToken cancellationToken,
        OperationContext? context = null,
        TimeSpan? waitTimeout = null,
        string? expectedSerial = null)
    {
        if (images.Count == 0)
        {
            throw new ArgumentException("ROOT 自动刷写至少需要一个镜像。", nameof(images));
        }

        if (context is null)
        {
            session.BeginOperation(OperationKind.Flashing, $"正在等待 {ToTargetLabel(target)} 设备");
            logs.Write(OperationLogLevel.Info, $"ROOT 自动流程正在等待 {ToTargetLabel(target)} 设备。 ");
        }
        else
        {
            context.ReportStage($"ROOT 自动流程正在等待 {ToTargetLabel(target)} 设备");
        }

        try
        {
            var device = await WaitForTargetAsync(target, cancellationToken, waitTimeout, expectedSerial);
            session.ApplyDevice(device);

            foreach (var (partition, image) in images)
            {
                var partitionName = ToPartitionName(partition);
                if (context is null)
                {
                    session.BeginOperation(OperationKind.Flashing, $"正在刷写 {partitionName}");
                    logs.Write(OperationLogLevel.Info, $"ROOT 自动流程正在刷写 {partitionName}: {Path.GetFileName(image.Path)}。");
                }
                else
                {
                    context.ReportStage($"ROOT 自动流程正在刷写 {partitionName}");
                }

                await cliRunner.FlashAsync(
                    device.Serial,
                    partitionName,
                    image.Path,
                    new Progress<double>(fraction => context?.ReportProgress(fraction)),
                    cancellationToken);
            }

            if (context is null)
            {
                session.BeginOperation(OperationKind.Rebooting, "ROOT 镜像刷写完成，正在重启设备");
            }
            else
            {
                context.ReportStage("ROOT 镜像刷写完成，正在重启设备");
            }

            await cliRunner.RebootAsync(device.Serial, cancellationToken);
            if (context is null)
            {
                session.CompleteOperation("ROOT 自动刷写完成，设备正在重启");
                logs.Write(OperationLogLevel.Success, "ROOT 自动流程已完成全部分区刷写，设备正在重启。 ");
            }
        }
        catch (OperationCanceledException)
        {
            if (context is null)
            {
                session.CancelOperation("ROOT 自动刷写已取消");
                logs.Write(OperationLogLevel.Warning, "ROOT 自动刷写已取消。 ");
            }

            throw;
        }
        catch (Exception exception)
        {
            if (context is null)
            {
                session.FailOperation("ROOT 自动刷写失败");
                logs.Write(OperationLogLevel.Error, exception.Message);
            }

            throw;
        }
    }

    /// <summary>构造分区刷写进度:同时上报到操作协调器(context)与 VM(flashProgress)。</summary>
    private static IProgress<double> CreateFlashProgress(
        OperationContext? context,
        IProgress<(string Partition, double Fraction)>? flashProgress,
        string partition) =>
        new Progress<double>(fraction =>
        {
            context?.ReportProgress(fraction);
            flashProgress?.Report((partition, fraction));
        });

    private void ReportWaiting(
        DeviceSessionViewModel session,
        FastbootTarget target,
        string partitionName,
        OperationContext? context)
    {
        if (context is null)
        {
            session.BeginOperation(OperationKind.Flashing, $"正在等待 {ToTargetLabel(target)} 设备");
            logs.Write(OperationLogLevel.Info, $"等待 {ToTargetLabel(target)} 设备，准备刷写 {partitionName}。 ");
            return;
        }

        context.ReportStage($"正在等待 {ToTargetLabel(target)} 设备");
    }

    private void ReportFlashing(
        DeviceSessionViewModel session,
        string partitionName,
        FlashImageInfo image,
        OperationContext? context)
    {
        if (context is null)
        {
            session.BeginOperation(OperationKind.Flashing, $"正在刷写 {partitionName}");
            logs.Write(OperationLogLevel.Info, $"开始刷写 {partitionName}: {Path.GetFileName(image.Path)}。");
            return;
        }

        context.ReportStage($"正在刷写 {partitionName}");
    }

    private Task<DeviceSnapshot> WaitForTargetAsync(FastbootTarget target, CancellationToken cancellationToken, TimeSpan? waitTimeout = null, string? expectedSerial = null) =>
        ResolveTargetAsync(target, waitForDevice: true, cancellationToken, waitTimeout, expectedSerial);

    private async Task<DeviceSnapshot> ResolveTargetAsync(
        FastbootTarget target,
        bool waitForDevice,
        CancellationToken cancellationToken,
        TimeSpan? waitTimeout = null,
        string? expectedSerial = null)
    {
        var deadline = waitTimeout is { } timeout ? DateTime.UtcNow + timeout : (DateTime?)null;
        while (true)
        {
            var device = await backend.DiscoverAsync(cancellationToken);
            if (await MatchesTargetAsync(device, target, cancellationToken, expectedSerial))
            {
                return device;
            }

            if (!waitForDevice)
            {
                throw new InvalidOperationException($"未检测到匹配的 {ToTargetLabel(target)} 设备。");
            }

            // 部分调用方(如全自动 ROOT)没有手动取消入口,必须给等待加超时,
            // 否则设备重启后没在 fastboot 现身会无限轮询、永久占用 operationGate。
            if (deadline is not null && DateTime.UtcNow >= deadline.Value)
            {
                throw new TimeoutException(
                    $"等待 {ToTargetLabel(target)} 设备超时（{waitTimeout!.Value.TotalSeconds:0} 秒），请确认设备已进入目标模式后重试。");
            }

            await Task.Delay(TimeSpan.FromSeconds(1), cancellationToken);
        }
    }

    private async Task<bool> MatchesTargetAsync(
        DeviceSnapshot device,
        FastbootTarget target,
        CancellationToken cancellationToken,
        string? expectedSerial = null)
    {
        if (device.ConnectionState != DeviceConnectionState.FastbootConnected)
        {
            return false;
        }

        // ROOT 自动流程绑定预检设备:序列号不一致(误连到另一台在 fastboot 的手机)
        // 绝不能刷,否则把 A 机修补的镜像刷进 B 机直接变砖。等待直到同一台现身。
        if (expectedSerial is not null && !string.Equals(device.Serial, expectedSerial, StringComparison.Ordinal))
        {
            return false;
        }

        string userspace;
        try
        {
            userspace = await cliRunner.GetVarAsync(device.Serial, "is-userspace", cancellationToken);
        }
        catch (OperationCanceledException)
        {
            throw;
        }
        catch
        {
            // 旧 bootloader 对 is-userspace 回答 FAIL unknown variable,CLI 抛错。
            // 按代码库惯例(DeviceInfoService.TryGetVarAsync / SafeFlashViewModel 都把
            // 可选 getvar 失败当良性条件)降级为“非 fastbootd”,让 Fastboot 目标能
            // 匹配上继续刷写,而不是抛异常摧毁整个等待循环。
            userspace = string.Empty;
        }

        var isFastbootd = IsTrueFastbootValue(userspace);
        return (target == FastbootTarget.Fastboot && !isFastbootd) ||
               (target == FastbootTarget.Fastbootd && isFastbootd);
    }

    private static string ToPartitionName(QuickFlashPartition partition) => partition switch
    {
        QuickFlashPartition.Boot => "boot",
        QuickFlashPartition.InitBoot => "init_boot",
        QuickFlashPartition.VendorBoot => "vendor_boot",
        QuickFlashPartition.Lk => "lk",
        _ => throw new ArgumentOutOfRangeException(nameof(partition), partition, "不支持的快速刷写分区。")
    };

    private static bool IsTrueFastbootValue(string value) =>
        value.Contains("yes", StringComparison.OrdinalIgnoreCase) ||
        value.Contains("true", StringComparison.OrdinalIgnoreCase);

    private static string NormalizeCurrentSlot(string value) => value.Trim().ToLowerInvariant() switch
    {
        "a" or "_a" => "a",
        "b" or "_b" => "b",
        _ => throw new InvalidOperationException("无法确定设备当前活动槽位。")
    };

    private static string ToTargetLabel(FastbootTarget target) => target == FastbootTarget.Fastbootd ? "fastbootd" : "fastboot";
}
