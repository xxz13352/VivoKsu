# VIVO 线刷刷写选项 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 给「VIVO 线刷」加刷写选项(清除数据 / 安全刷写 / 保留ROOT / 槽位三选一 / 回锁BL 预留),并把所有用户可见日志与文案模糊化(不暴露分区名与技术机制)。

**Architecture:** `FirmwarePartitionExtractor` 变纯目录(返回全量分区),`SafeFlashViewModel` 按选项过滤;槽位目标由纯函数 `SafeFlashSlotPlanner` 计算;`wipe-data.img` 作为标准 .NET 嵌入资源随程序集携带,刷写收尾经 `EmbeddedWipeData` 解出并 `fastboot flash misc`。所有新增逻辑均可脱离设备单测。

**Tech Stack:** .NET 8 WPF,CommunityToolkit.Mvvm 8.4,HandyControl 3.5.1,xunit + FluentAssertions。

## Global Constraints

- 命名:客户端 UI 显示「奶蛙Flash」;代码/工程/服务名 `NWflash`,缩写 `NWF`;域名 `nwflash.cc.cd`。
- **日志/文案模糊化是硬性要求**:用户可见文本不得出现分区名、`preloader`/`lk`、`misc`、`wipe-data`、`set_active`、槽位名(`_a`/`_b`)等技术细节。
- 测试命令:`dotnet test tests/VivoKsu.App.Tests/VivoKsu.App.Tests.csproj -c Debug`。
- 构建命令:`dotnet build src/VivoKsu.App/VivoKsu.App.csproj -c Debug`。
- 每个任务结束时全部测试必须通过(`0 失败`),测试基数当前 **397**。
- 提交信息用中文前缀(`feat:`/`refactor:`/`test:` 等),末尾加 `Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>`。
- 不修改 `VivoKsu.Bootstrapper`(无 AOT 变更);发布脚本 `Publish-Release.ps1` 不受影响。

---

### Task 1: 内嵌 wipe-data 资源 + EmbeddedWipeData 助手

**Files:**
- Create: `src/VivoKsu.App/Assets/wipe-data.img`(复制自 `C:\Users\17254\Downloads\wipe-data.img`,512KB)
- Modify: `src/VivoKsu.App/VivoKsu.App.csproj`
- Create: `src/VivoKsu.App/Services/EmbeddedWipeData.cs`
- Test: `tests/VivoKsu.App.Tests/EmbeddedWipeDataTests.cs`

**Interfaces:**
- Consumes: 无
- Produces: `internal static class EmbeddedWipeData` — `static Task<long> WriteToAsync(string destinationPath, CancellationToken cancellationToken)`,返回写入字节数;资源缺失时抛 `InvalidOperationException`(消息模糊)。

- [ ] **Step 1: 复制 wipe-data.img 进 Assets**

```bash
mkdir -p "src/VivoKsu.App/Assets" && cp "C:/Users/17254/Downloads/wipe-data.img" "src/VivoKsu.App/Assets/wipe-data.img" && ls -la "src/VivoKsu.App/Assets/wipe-data.img"
```
Expected: 524288 字节文件出现在 `src/VivoKsu.App/Assets/wipe-data.img`。

- [ ] **Step 2: csproj 注册为嵌入资源**

在 `src/VivoKsu.App/VivoKsu.App.csproj` 的 logo `<Resource>` ItemGroup 后追加一个 ItemGroup:

```xml
<ItemGroup>
    <!-- wipe-data 镜像:数据清除用,嵌入程序集(标准 .NET 资源,非 WPF pack URI) -->
    <EmbeddedResource Include="Assets\wipe-data.img" />
</ItemGroup>
```

默认逻辑资源名:`VivoKsu.App.Assets.wipe-data.img`。

- [ ] **Step 3: 写 EmbeddedWipeData 助手**

创建 `src/VivoKsu.App/Services/EmbeddedWipeData.cs`:

```csharp
using System.IO;
using System.Reflection;

namespace VivoKsu.App.Services;

/// <summary>把嵌入程序集的 wipe-data 镜像解到磁盘文件,供刷写 misc 分区用。异常消息保持模糊。</summary>
internal static class EmbeddedWipeData
{
    /// <summary>把内嵌 wipe-data 镜像写入 destinationPath,返回写入字节数。</summary>
    public static async Task<long> WriteToAsync(string destinationPath, CancellationToken cancellationToken)
    {
        var assembly = typeof(EmbeddedWipeData).Assembly;
        var resourceName = assembly.GetManifestResourceNames()
            .FirstOrDefault(name => name.EndsWith("wipe-data.img", StringComparison.OrdinalIgnoreCase))
            ?? throw new InvalidOperationException("数据清除资源不可用。");
        await using var input = assembly.GetManifestResourceStream(resourceName)
            ?? throw new InvalidOperationException("数据清除资源不可用。");
        await using (var output = new FileStream(destinationPath, FileMode.Create, FileAccess.Write, FileShare.None, 1 << 20, useAsync: true))
        {
            await input.CopyToAsync(output, cancellationToken);
        }

        return new FileInfo(destinationPath).Length;
    }
}
```

- [ ] **Step 4: 写测试**

创建 `tests/VivoKsu.App.Tests/EmbeddedWipeDataTests.cs`:

```csharp
using FluentAssertions;
using VivoKsu.App.Services;

namespace VivoKsu.App.Tests;

public class EmbeddedWipeDataTests
{
    [Fact]
    public async Task WriteToAsync_extracts_the_embedded_wipe_data_image_to_disk()
    {
        var directory = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(directory);
        try
        {
            var destination = Path.Combine(directory, "wipe-data.img");

            var bytes = await EmbeddedWipeData.WriteToAsync(destination, CancellationToken.None);

            bytes.Should().Be(524288);
            new FileInfo(destination).Length.Should().Be(524288);
        }
        finally
        {
            Directory.Delete(directory, recursive: true);
        }
    }
}
```

- [ ] **Step 5: 跑测试验证通过**

Run: `dotnet test tests/VivoKsu.App.Tests/VivoKsu.App.Tests.csproj -c Debug --filter EmbeddedWipeDataTests`
Expected: 通过(新增 EmbeddedWipeDataTests 全绿,0 失败)。若资源名匹配失败,先跑 `dotnet build` 再用反射列出 `GetManifestResourceNames()` 核对。

- [ ] **Step 6: 提交**

```bash
git add src/VivoKsu.App/Assets/wipe-data.img src/VivoKsu.App/VivoKsu.App.csproj src/VivoKsu.App/Services/EmbeddedWipeData.cs tests/VivoKsu.App.Tests/EmbeddedWipeDataTests.cs
git commit -m "feat(线刷): 内嵌 wipe-data 镜像资源 + EmbeddedWipeData 解出助手

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: SafeFlashSlotMode 枚举 + SafeFlashSlotPlanner 纯函数

**Files:**
- Create: `src/VivoKsu.App/Models/SafeFlashSlotMode.cs`
- Create: `src/VivoKsu.App/Services/SafeFlashSlotPlanner.cs`
- Test: `tests/VivoKsu.App.Tests/SafeFlashSlotPlannerTests.cs`

**Interfaces:**
- Consumes: 无
- Produces:
  - `public enum SafeFlashSlotMode { CurrentSlot, OtherSlot, BothSlots }`(Models 命名空间)
  - `public static class SafeFlashSlotPlanner`
    - `static bool IsSlotBasedMode(SafeFlashSlotMode mode)` — `mode != CurrentSlot`
    - `static string[] ComputeTargets(string partitionName, SafeFlashSlotMode mode, string? currentSlot, bool hasSlot)`
    - `static string? OtherSlot(string? currentSlot)` — `"a"→"b"`、`"_a"→"b"`、`"b"→"a"`、其余/null→null

- [ ] **Step 1: 写测试(先红)**

创建 `tests/VivoKsu.App.Tests/SafeFlashSlotPlannerTests.cs`:

```csharp
using FluentAssertions;
using VivoKsu.App.Models;
using VivoKsu.App.Services;

namespace VivoKsu.App.Tests;

public class SafeFlashSlotPlannerTests
{
    [Theory]
    [InlineData("boot", SafeFlashSlotMode.CurrentSlot, "a", true, "boot")]
    [InlineData("boot", SafeFlashSlotMode.OtherSlot, "a", true, "boot_b")]
    [InlineData("boot", SafeFlashSlotMode.OtherSlot, "b", true, "boot_a")]
    [InlineData("boot", SafeFlashSlotMode.OtherSlot, null, true, "boot")]
    [InlineData("boot", SafeFlashSlotMode.OtherSlot, "a", false, "boot")]
    [InlineData("boot", SafeFlashSlotMode.BothSlots, "a", true, "boot_a", "boot_b")]
    [InlineData("boot", SafeFlashSlotMode.BothSlots, "a", false, "boot")]
    public void ComputeTargets_returns_expected_targets(
        string partition, SafeFlashSlotMode mode, string? currentSlot, bool hasSlot, params string[] expected)
    {
        SafeFlashSlotPlanner.ComputeTargets(partition, mode, currentSlot, hasSlot)
            .Should().BeEquivalentTo(expected);
    }

    [Theory]
    [InlineData("a", "b")]
    [InlineData("b", "a")]
    [InlineData("_a", "b")]
    [InlineData(null, null)]
    [InlineData("", null)]
    [InlineData("c", null)]
    public void OtherSlot_maps_a_and_b_only(string? current, string? expected)
    {
        SafeFlashSlotPlanner.OtherSlot(current).Should().Be(expected);
    }

    [Theory]
    [InlineData(SafeFlashSlotMode.CurrentSlot, false)]
    [InlineData(SafeFlashSlotMode.OtherSlot, true)]
    [InlineData(SafeFlashSlotMode.BothSlots, true)]
    public void IsSlotBasedMode_is_false_only_for_current_slot(SafeFlashSlotMode mode, bool expected)
    {
        SafeFlashSlotPlanner.IsSlotBasedMode(mode).Should().Be(expected);
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `dotnet test tests/VivoKsu.App.Tests/VivoKsu.App.Tests.csproj -c Debug --filter SafeFlashSlotPlannerTests`
Expected: 编译失败(`SafeFlashSlotMode` / `SafeFlashSlotPlanner` 未定义)。

- [ ] **Step 3: 写枚举**

创建 `src/VivoKsu.App/Models/SafeFlashSlotMode.cs`:

```csharp
namespace VivoKsu.App.Models;

/// <summary>VIVO 线刷刷写槽位目标(双槽设备)。CurrentSlot 为默认,零额外设备查询。</summary>
public enum SafeFlashSlotMode
{
    /// <summary>刷入当前活动槽(默认;fastboot 自动用当前槽)。</summary>
    CurrentSlot,

    /// <summary>刷入另一侧槽位,刷完后切到该槽启动。</summary>
    OtherSlot,

    /// <summary>两侧槽位都刷入。</summary>
    BothSlots
}
```

- [ ] **Step 4: 写 SafeFlashSlotPlanner**

创建 `src/VivoKsu.App/Services/SafeFlashSlotPlanner.cs`:

```csharp
using VivoKsu.App.Models;

namespace VivoKsu.App.Services;

/// <summary>
/// VIVO 线刷槽位目标规划(纯函数,可单测)。
/// 约定:非槽位分区(has-slot:false)或设备非 A/B 时,目标回退为分区原名,保证安全降级不砖机。
/// </summary>
public static class SafeFlashSlotPlanner
{
    /// <summary>是否需要对设备做槽位探测(has-slot / current-slot)的模式。</summary>
    public static bool IsSlotBasedMode(SafeFlashSlotMode mode) => mode != SafeFlashSlotMode.CurrentSlot;

    /// <summary>按模式与设备信息计算某分区要刷入的目标分区名数组。</summary>
    public static string[] ComputeTargets(
        string partitionName,
        SafeFlashSlotMode mode,
        string? currentSlot,
        bool hasSlot)
    {
        if (!hasSlot)
        {
            return [partitionName];
        }

        return mode switch
        {
            SafeFlashSlotMode.OtherSlot => [TargetForSlot(partitionName, OtherSlot(currentSlot))],
            SafeFlashSlotMode.BothSlots => [TargetForSlot(partitionName, "a"), TargetForSlot(partitionName, "b")],
            _ => [partitionName]
        };
    }

    /// <summary>当前槽的对侧槽位("a"→"b"、"b"→"a"),读不到/异常返回 null。</summary>
    public static string? OtherSlot(string? currentSlot) =>
        currentSlot?.Trim().ToLowerInvariant() switch
        {
            "a" or "_a" => "b",
            "b" or "_b" => "a",
            _ => null
        };

    private static string TargetForSlot(string partitionName, string? slot) =>
        slot is null ? partitionName : $"{partitionName}_{slot}";
}
```

- [ ] **Step 5: 跑测试验证通过**

Run: `dotnet test tests/VivoKsu.App.Tests/VivoKsu.App.Tests.csproj -c Debug --filter SafeFlashSlotPlannerTests`
Expected: 全部通过。

- [ ] **Step 6: 提交**

```bash
git add src/VivoKsu.App/Models/SafeFlashSlotMode.cs src/VivoKsu.App/Services/SafeFlashSlotPlanner.cs tests/VivoKsu.App.Tests/SafeFlashSlotPlannerTests.cs
git commit -m "feat(线刷): SafeFlashSlotMode 枚举 + SafeFlashSlotPlanner 槽位目标纯函数

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: 提取器变纯目录 + VM 选项属性 / 分区过滤 / 确认文案模糊

**Files:**
- Modify: `src/VivoKsu.App/Services/FirmwarePartitionExtractor.cs`
- Modify: `src/VivoKsu.App/ViewModels/SafeFlashViewModel.cs`
- Test: `tests/VivoKsu.App.Tests/FirmwarePartitionExtractorTests.cs`
- Test: `tests/VivoKsu.App.Tests/SafeFlashViewModelTests.cs`

**Interfaces:**
- Consumes: `FirmwarePartitionExtractor.ShouldSkip(string)`(保留,静态);`SafeFlashSlotMode`(Task 2);`PayloadPartitionEntry(Name, SizeBytes, CompressionType)`。
- Produces(VM 新增成员,Task 4 与 XAML 依赖):
  - `[ObservableProperty] bool isWipeData / isSafeFlash(=true) / isKeepRoot` → 公开属性 `IsWipeData` / `IsSafeFlash` / `IsKeepRoot`
  - `SafeFlashSlotMode SlotMode { get; set; }`(默认 `CurrentSlot`)
  - `bool IsSlotCurrent / IsSlotOther / IsSlotBoth { get; set; }`(单向 setter 写 `SlotMode`)
  - `bool IsOptionsEnabled => !IsBusy && !IsConfirmVisible`
  - `SetPendingSourceForTesting` 改为按选项过滤后再写入
  - `private bool IsPartitionIncluded(string name)`、`private static bool IsBootPartition(string name)`

- [ ] **Step 1: 提取器去掉 lk/preloader 过滤**

`src/VivoKsu.App/Services/FirmwarePartitionExtractor.cs` 三处过滤改为返回全量:
- 目录源(现 66-68 行):删掉 `.Where(entry => !ShouldSkip(entry.Name))`,直接 `return ListDirectoryImages(source).ToList();`
- 直接镜像 zip(现 84-85 行):删掉 `.Where(entry => !ShouldSkip(Path.GetFileNameWithoutExtension(entry.FullName)))`
- payload 源(现 99 行):`return partitions.Where(partition => !ShouldSkip(partition.Name)).ToList();` → `return partitions.ToList();`

类 XML 注释(现 15 行)改为:`/// 返回固件内的全量可刷镜像(含引导加载器);是否剔除由上层选项决定。`。`ShouldSkip(string)` 方法本体保留(VM 仍调用)。

- [ ] **Step 2: VM 加选项属性与过滤谓词**

在 `src/VivoKsu.App/ViewModels/SafeFlashViewModel.cs` 的 `[ObservableProperty]` 区(现有 `confirmSummary` 之后)追加:

```csharp
    [ObservableProperty]
    private bool isWipeData;

    [ObservableProperty]
    private bool isSafeFlash = true;

    [ObservableProperty]
    private bool isKeepRoot;

    private SafeFlashSlotMode slotMode = SafeFlashSlotMode.CurrentSlot;

    public SafeFlashSlotMode SlotMode
    {
        get => slotMode;
        set
        {
            if (SetProperty(ref slotMode, value))
            {
                OnPropertyChanged(nameof(IsSlotCurrent));
                OnPropertyChanged(nameof(IsSlotOther));
                OnPropertyChanged(nameof(IsSlotBoth));
            }
        }
    }

    public bool IsSlotCurrent { get => SlotMode == SafeFlashSlotMode.CurrentSlot; set { if (value) SlotMode = SafeFlashSlotMode.CurrentSlot; } }

    public bool IsSlotOther { get => SlotMode == SafeFlashSlotMode.OtherSlot; set { if (value) SlotMode = SafeFlashSlotMode.OtherSlot; } }

    public bool IsSlotBoth { get => SlotMode == SafeFlashSlotMode.BothSlots; set { if (value) SlotMode = SafeFlashSlotMode.BothSlots; } }

    /// <summary>刷写选项行可用性:忙碌或确认面板出现后锁定,避免计划与确认不一致。</summary>
    public bool IsOptionsEnabled => !IsBusy && !IsConfirmVisible;
```

在类内新增过滤谓词(放在 `ParseBbkVersion` 附近):

```csharp
    /// <summary>保留ROOT 要跳过的启动分区(boot/init_boot/vendor_boot)。</summary>
    private static bool IsBootPartition(string name) =>
        name.Equals("boot", StringComparison.OrdinalIgnoreCase) ||
        name.Equals("init_boot", StringComparison.OrdinalIgnoreCase) ||
        name.Equals("vendor_boot", StringComparison.OrdinalIgnoreCase);

    /// <summary>按选项决定某分区是否纳入刷写:安全刷写排除引导加载器,保留ROOT 排除启动分区。</summary>
    private bool IsPartitionIncluded(string name) =>
        (!IsSafeFlash || !FirmwarePartitionExtractor.ShouldSkip(name)) &&
        (!IsKeepRoot || !IsBootPartition(name));
```

在 `OnIsBusyChanged` 与 `OnIsConfirmVisibleChanged` 里补一行通知选项可用性:
`OnPropertyChanged(nameof(IsOptionsEnabled));`

- [ ] **Step 3: SetPendingSourceForTesting 按选项过滤**

改 `src/VivoKsu.App/ViewModels/SafeFlashViewModel.cs` 的 `SetPendingSourceForTesting`(现 782-792 行):

```csharp
    internal void SetPendingSourceForTesting(
        string source,
        string stagingRoot,
        IReadOnlyList<PayloadPartitionEntry> partitions)
    {
        sourcePath = source;
        this.stagingRoot = stagingRoot;
        pendingPartitions = partitions.Where(partition => IsPartitionIncluded(partition.Name)).ToList();
        FlashCount = pendingPartitions.Count;
        IsConfirmVisible = true;
    }
```

- [ ] **Step 4: PrepareFlashAsync 过滤 + 确认文案模糊**

`src/VivoKsu.App/ViewModels/SafeFlashViewModel.cs` 的 `PrepareFlashAsync`(现 320-360 行)改为:

```csharp
    private async Task PrepareFlashAsync()
    {
        var success = await RunOperationAsync(OperationKind.Hashing, "分析固件", async (context, ct) =>
        {
            context.ReportStage("正在读取分区列表");
            var partitions = await extractor.ListPartitionsAsync(sourcePath, ct);
            pendingPartitions = partitions.Where(partition => IsPartitionIncluded(partition.Name)).ToList();
            FlashCount = pendingPartitions.Count;
            context.ReportProgress(1);
        });
        if (!success)
        {
            CleanupStaging();
            return;
        }

        if (FlashCount == 0)
        {
            StatusText = "固件中未找到可刷写分区。";
            logs.Write(OperationLogLevel.Warning, StatusText);
            CleanupStaging();
            return;
        }

        logs.Write(OperationLogLevel.Info, $"固件分析完成,共 {FlashCount} 个分区");

        ConfirmSummary = $"将刷入 {FlashCount} 个分区,随后重启到 fastbootd 逐个刷写。";
        if (!IsSafeFlash)
        {
            ConfirmSummary += Environment.NewLine + "⚠ 未启用安全模式,将完整写入固件,存在风险。";
        }

        if (IsKeepRoot)
        {
            ConfirmSummary += Environment.NewLine + "将保留现有启动状态。";
        }

        if (IsWipeData)
        {
            ConfirmSummary += Environment.NewLine + "完成后将清除设备数据。";
        }

        switch (SlotMode)
        {
            case SafeFlashSlotMode.OtherSlot:
                ConfirmSummary += Environment.NewLine + "完成后将从另一侧启动。";
                break;
            case SafeFlashSlotMode.BothSlots:
                ConfirmSummary += Environment.NewLine + "将写入设备两侧。";
                break;
        }

        if (extractor.HasBlockBasedContent(sourcePath))
        {
            var blockWarning = "⚠ 固件含块式分区内容(.new.dat / transfer.list,如 system/vendor/product),暂不支持刷写,本次只会刷可直接镜像的分区,其余保持原样。";
            logs.Write(OperationLogLevel.Warning, blockWarning);
            ConfirmSummary += Environment.NewLine + blockWarning;
        }

        IsConfirmVisible = true;
    }
```

注意:原「逐分区打印 `{partition.Name}.img > {partition.Name} | size`」的循环(现 346-349 行)**删除**(暴露分区名)。

- [ ] **Step 5: 更新提取器测试**

`tests/VivoKsu.App.Tests/FirmwarePartitionExtractorTests.cs`:
- `ListPartitionsAsync_on_direct_image_zip_filters_preloader_lk_and_non_images`(26 行)→ 改名 `ListPartitionsAsync_on_direct_image_zip_lists_all_images`;断言(41 行)改为 `Should().BeEquivalentTo(["boot", "lk", "preloader", "preloader_emmc"])`(仍排除 `system.new.dat` 与 `META-INF/...`)。
- `ListPartitionsAsync_on_an_extracted_folder_lists_images_and_filters_preloader_lk`(132 行)→ 改名 `..._lists_all_images`;断言(145 行)改为 `Should().BeEquivalentTo(["boot", "lk", "preloader"])`。
- `ShouldSkip_filters_preloader_and_lk`(20 行)保留不动。

- [ ] **Step 6: 更新 SafeFlash 测试(过滤相关的)**

`tests/VivoKsu.App.Tests/SafeFlashViewModelTests.cs`:现有用例 `ConfirmFlashAsync_extracts_and_flashes_all_partitions_except_preloader_and_lk`(12 行)此时**应仍通过**(SetPendingSourceForTesting 过滤兜底),无需改。追加两个新用例(放在该类末尾、`CreateViewModel` 之前):

```csharp
    [Fact]
    public async Task ConfirmFlashAsync_safe_flash_off_flashes_preloader_and_lk()
    {
        var directory = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(directory);
        try
        {
            var zip = Path.Combine(directory, "ota.zip");
            using (var archive = ZipFile.Open(zip, ZipArchiveMode.Create))
            {
                CreateEntry(archive, "boot.img", [0x01]);
                CreateEntry(archive, "lk.img", [0x02]);
                CreateEntry(archive, "preloader.img", [0x03]);
            }

            var session = new DeviceSessionViewModel();
            session.ApplyDevice(new DeviceSnapshot(
                DeviceConnectionState.FastbootConnected, "FB123", "fastboot 已连接", "vivo"));
            var logs = new OperationLogService();
            var fake = new FakeFastbootCliRunner();
            var viewModel = CreateViewModel(session, new FlashApi(), logs, fake);
            viewModel.IsSafeFlash = false;
            var extractor = new FirmwarePartitionExtractor(payloadDumper: null);
            var partitions = await extractor.ListPartitionsAsync(zip, CancellationToken.None);
            viewModel.SetPendingSourceForTesting(zip, Path.Combine(directory, "staging"), partitions);

            await viewModel.ConfirmFlashCommand.ExecuteAsync(null);

            fake.FlashRequests.Select(request => request.Partition)
                .Should().BeEquivalentTo(["boot", "lk", "preloader"]);
        }
        finally
        {
            Directory.Delete(directory, recursive: true);
        }
    }

    [Fact]
    public async Task ConfirmFlashAsync_keep_root_skips_boot_partitions()
    {
        var directory = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(directory);
        try
        {
            var zip = Path.Combine(directory, "ota.zip");
            using (var archive = ZipFile.Open(zip, ZipArchiveMode.Create))
            {
                CreateEntry(archive, "boot.img", [0x01]);
                CreateEntry(archive, "init_boot.img", [0x02]);
                CreateEntry(archive, "vendor_boot.img", [0x03]);
                CreateEntry(archive, "system.img", [0x04]);
            }

            var session = new DeviceSessionViewModel();
            session.ApplyDevice(new DeviceSnapshot(
                DeviceConnectionState.FastbootConnected, "FB123", "fastboot 已连接", "vivo"));
            var logs = new OperationLogService();
            var fake = new FakeFastbootCliRunner();
            var viewModel = CreateViewModel(session, new FlashApi(), logs, fake);
            viewModel.IsKeepRoot = true;
            var extractor = new FirmwarePartitionExtractor(payloadDumper: null);
            var partitions = await extractor.ListPartitionsAsync(zip, CancellationToken.None);
            viewModel.SetPendingSourceForTesting(zip, Path.Combine(directory, "staging"), partitions);

            await viewModel.ConfirmFlashCommand.ExecuteAsync(null);

            fake.FlashRequests.Select(request => request.Partition).Should().BeEquivalentTo(["system"]);
        }
        finally
        {
            Directory.Delete(directory, recursive: true);
        }
    }
```

- [ ] **Step 7: 全量跑测试**

Run: `dotnet test tests/VivoKsu.App.Tests/VivoKsu.App.Tests.csproj -c Debug`
Expected: 通过(新增 2 用例全绿,0 失败)。若 `keep_root` 用例闪到 `boot` 等,说明 `IsBootPartition` 精确匹配漏了大小写/名称,核对谓词。

- [ ] **Step 8: 提交**

```bash
git add src/VivoKsu.App/Services/FirmwarePartitionExtractor.cs src/VivoKsu.App/ViewModels/SafeFlashViewModel.cs tests/VivoKsu.App.Tests/FirmwarePartitionExtractorTests.cs tests/VivoKsu.App.Tests/SafeFlashViewModelTests.cs
git commit -m "feat(线刷): 提取器变纯目录 + 选项属性/分区过滤(安全刷写·保留ROOT)+ 确认文案模糊化

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

---

### Task 4: 刷写循环——槽位目标 + 清除数据 + 日志模糊化

**Files:**
- Modify: `src/VivoKsu.App/ViewModels/SafeFlashViewModel.cs`
- Test: `tests/VivoKsu.App.Tests/SafeFlashViewModelTests.cs`

**Interfaces:**
- Consumes: `SafeFlashSlotPlanner`(Task 2)、`EmbeddedWipeData`(Task 1)、`cliRunner`(`IFastbootCliRunner.GetVarAsync` / `SetActiveAsync` / `FlashAsync` / `RebootAsync` / `PartitionExistsAsync`)、Task 3 的 VM 选项属性。
- Produces: 修改 `ConfirmFlashAsync` 的刷写循环、`CurrentPartition` 显示、`StatusText`、`RetainExtractedForResume` 提示与各日志文案。

- [ ] **Step 1: 解包阶段模糊化**

`src/VivoKsu.App/ViewModels/SafeFlashViewModel.cs` 的 `ConfirmFlashAsync` 解包循环:
- `CurrentPartition = partition.Name;` → `CurrentPartition = $"分区 {index + 1}/{partitionsToFlash.Count}";`
- `context.ReportStage($"正在解包 {partition.Name}({index + 1}/{partitionsToFlash.Count})", ...)` → `context.ReportStage($"正在解包分区 {index + 1}/{partitionsToFlash.Count}", ...)`

- [ ] **Step 2: 加清除数据准备 + 槽位读取助手**

`ConfirmFlashAsync` 的 `RunOperationAsync` 回调开头(解包前)插入:

```csharp
            // 清除数据:先解出内嵌 wipe-data 镜像,尽早失败(避免刷一半才发现资源缺失)。
            string? wipeDataPath = null;
            if (IsWipeData)
            {
                wipeDataPath = Path.Combine(extractDirectory, "wipe-data.img");
                context.ReportStage("正在准备数据清除");
                await EmbeddedWipeData.WriteToAsync(wipeDataPath, ct);
            }
```

在类内新增两个助手(放在 `PartitionExistsAsync` 附近):

```csharp
    /// <summary>读当前活动槽位(a/b);读不到/非 a/b 返回 null(非 A/B 设备安全降级)。</summary>
    private async Task<string?> ReadCurrentSlotAsync(string serial, CancellationToken cancellationToken)
    {
        try
        {
            var value = await cliRunner.GetVarAsync(serial, "current-slot", cancellationToken);
            var slot = value?.Trim().TrimStart('_').ToLowerInvariant();
            return slot is "a" or "b" ? slot : null;
        }
        catch
        {
            return null;
        }
    }

    /// <summary>分区是否有槽位(has-slot);查询失败按 false 处理(回退原样刷写)。</summary>
    private async Task<bool> HasSlotAsync(string serial, string partition, CancellationToken cancellationToken)
    {
        try
        {
            var value = await cliRunner.GetVarAsync(serial, $"has-slot:{partition}", cancellationToken);
            return value?.Trim().ToLowerInvariant() is "yes" or "true" or "1";
        }
        catch
        {
            return false;
        }
    }
```

- [ ] **Step 3: 刷写循环改为槽位目标**

把 `ConfirmFlashAsync` 的刷写循环(现 439-483 行)整体替换为:

```csharp
            // 3. 逐个刷入(目标分区名由槽位模式决定)。用当前 session.Serial,
            //    并依赖 fastboot 调用自身在设备断开时报错——不额外校验会话状态,
            //    避免长时间刷大分区时监控瞬时抖动导致误中止。
            var serial = session.Serial;
            var currentSlot = SafeFlashSlotPlanner.IsSlotBasedMode(SlotMode)
                ? await ReadCurrentSlotAsync(serial, ct)
                : null;
            var skipped = 0;
            for (var index = 0; index < images.Count; index++)
            {
                ct.ThrowIfCancellationRequested();
                if (string.IsNullOrWhiteSpace(serial) || session.ConnectionState != DeviceConnectionState.FastbootConnected)
                {
                    throw new InvalidOperationException("刷写过程中设备连接已断开。");
                }

                var image = images[index];
                // 当前槽模式:不查 has-slot,目标即分区名,行为与原来完全一致。
                string[] targets = SafeFlashSlotPlanner.IsSlotBasedMode(SlotMode)
                    ? SafeFlashSlotPlanner.ComputeTargets(
                        image.PartitionName, SlotMode, currentSlot,
                        await HasSlotAsync(serial, image.PartitionName, ct))
                    : [image.PartitionName];

                foreach (var target in targets)
                {
                    // 设备上不存在的分区先 getvar 探一下,不存在就跳过,避免未知分区中止整条流程。
                    if (!await PartitionExistsAsync(serial, target, ct))
                    {
                        logs.Write(OperationLogLevel.Warning, "有 1 个分区不可用,已跳过。");
                        skipped++;
                        continue;
                    }

                    CurrentPartition = $"分区 {index + 1}/{images.Count}";
                    // fastboot CLI 带连续传输进度(进程写字节/镜像大小)。
                    CurrentPartitionProgress = 0;
                    lastExtractSpeedBytes = 0;
                    lastExtractSpeedTicks = 0;
                    SpeedText = string.Empty;
                    context.ReportStage($"正在刷写分区 {index + 1}/{images.Count}", OperationKind.Flashing);
                    var flashWatch = Stopwatch.StartNew();
                    var flashProgress = new Progress<double>(fraction =>
                    {
                        CurrentPartitionProgress = fraction;
                        OverallProgress = 0.5 + ((index + fraction) / images.Count) * 0.5;
                        UpdateExtractSpeed((long)(fraction * image.SizeBytes));
                    });
                    await cliRunner.FlashAsync(serial, target, image.ImagePath, flashProgress, ct);
                    flashWatch.Stop();
                    logs.Write(OperationLogLevel.Info, $"分区 {index + 1}/{images.Count} 写入完成");
                    OverallProgress = 0.5 + ((index + 1) / (double)images.Count) * 0.5;
                }
            }

            // 4. 对槽:切到对槽启动(仅当能确定对槽)。
            if (SlotMode == SafeFlashSlotMode.OtherSlot)
            {
                var otherSlot = SafeFlashSlotPlanner.OtherSlot(currentSlot);
                if (otherSlot is not null)
                {
                    await cliRunner.SetActiveAsync(serial, otherSlot, ct);
                }
            }

            // 5. 清除数据:最后写入 misc(触发开机数据清除)。
            if (IsWipeData && wipeDataPath is not null)
            {
                context.ReportStage("正在执行数据清除");
                logs.Write(OperationLogLevel.Info, "正在执行数据清除");
                if (!await PartitionExistsAsync(serial, "misc", ct))
                {
                    logs.Write(OperationLogLevel.Warning, "数据清除未完成,misc 分区不可用。");
                }
                else
                {
                    await cliRunner.FlashAsync(serial, "misc", wipeDataPath, null, ct);
                    logs.Write(OperationLogLevel.Info, "数据清除完成");
                }
            }

            // 6. 重启回系统。
            logs.Write(OperationLogLevel.Info, "[Rebooting]发送重启命令...");
            context.ReportStage("正在重启设备");
            await cliRunner.RebootAsync(serial, ct);
```

注意:原循环里 `logs.Write(OperationLogLevel.Info, $"已连接 {session.Serial} | 用时 {taskStopwatch.Elapsed.TotalSeconds:0} 秒")` → 改为 `$"已连接设备 | 用时 {taskStopwatch.Elapsed.TotalSeconds:0} 秒"`(在设备进入 fastbootd 后的分支里)。原 `Sending 'boot'` / `OKAY [...]` / `Writing` / `Finished. Total time` / `Flashing boot.img...OK` 五行逐分区日志全部删除,由 `分区 {i}/{n} 写入完成` 一行取代。

- [ ] **Step 4: 收尾文案模糊**

`ConfirmFlashAsync` 尾部 `StatusText` 与其它文案:
- `StatusText = skipped > 0 ? $"已刷入 {images.Count - skipped} 个分区,跳过 {skipped} 个设备不存在的分区" : $"已刷入 {images.Count} 个分区";` → `$"已刷入 {images.Count - skipped} 个分区,已跳过 {skipped} 个不可用分区"`(统一,含 skipped==0 时也一致;保留两个分支亦可,但都用「不可用分区」措辞)。
- `DownloadAndFlashAsync` / `SelectAndFlashAsync` / `SelectFolderAndFlashAsync` 里的 `logs.Write(..., $"已选择固件 {sourcePath}")` → `logs.Write(OperationLogLevel.Info, "已选择固件")`(去路径,三处)。
- `RetainExtractedForResume` 文案保留(恢复提示需路径),但其中的「选择解包文件夹」指引不变。

- [ ] **Step 5: 更新既有测试断言**

`tests/VivoKsu.App.Tests/SafeFlashViewModelTests.cs`:
- 用例 `ConfirmFlashAsync_extracts_and_flashes_all_partitions_except_preloader_and_lk`(12 行)末尾(42-43 行)断言改为:
  ```csharp
  logs.Entries.Should().Contain(entry => entry.Message.Contains("分区 1/1 写入完成"));
  logs.Entries.Should().Contain(entry => entry.Message.Contains("任务结束"));
  ```
  (删掉 `Sending 'boot'` / `Finished. Total time:` 断言。)
- 用例 `ConfirmFlashAsync_waits_for_fastbootd_after_adb_reboot`(126 行)末尾(161 行)断言 `"已连接 FB123"` → `"已连接设备"`。

- [ ] **Step 6: 追加槽位/清除数据用例**

在 `tests/VivoKsu.App.Tests/SafeFlashViewModelTests.cs` 末尾、`CreateViewModel` 之前追加:

```csharp
    [Fact]
    public async Task ConfirmFlashAsync_other_slot_flashes_target_slot_and_switches_active()
    {
        var directory = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(directory);
        try
        {
            var zip = Path.Combine(directory, "ota.zip");
            using (var archive = ZipFile.Open(zip, ZipArchiveMode.Create))
            {
                CreateEntry(archive, "boot.img", [0x01]);
            }

            var session = new DeviceSessionViewModel();
            session.ApplyDevice(new DeviceSnapshot(
                DeviceConnectionState.FastbootConnected, "FB123", "fastboot 已连接", "vivo"));
            var logs = new OperationLogService();
            var fake = new FakeFastbootCliRunner
            {
                GetVarHandler = variable => variable switch
                {
                    "current-slot" => "a",
                    _ when variable.StartsWith("has-slot:", StringComparison.OrdinalIgnoreCase) => "yes",
                    _ => string.Empty
                }
            };
            var viewModel = CreateViewModel(session, new FlashApi(), logs, fake);
            viewModel.SlotMode = SafeFlashSlotMode.OtherSlot;
            var extractor = new FirmwarePartitionExtractor(payloadDumper: null);
            var partitions = await extractor.ListPartitionsAsync(zip, CancellationToken.None);
            viewModel.SetPendingSourceForTesting(zip, Path.Combine(directory, "staging"), partitions);

            await viewModel.ConfirmFlashCommand.ExecuteAsync(null);

            fake.FlashRequests.Select(request => request.Partition).Should().BeEquivalentTo(["boot_b"]);
            fake.SetActiveSlots.Should().Contain("b");
            fake.Rebooted.Should().Contain("FB123");
        }
        finally
        {
            Directory.Delete(directory, recursive: true);
        }
    }

    [Fact]
    public async Task ConfirmFlashAsync_both_slots_flashes_a_and_b_once_each()
    {
        var directory = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(directory);
        try
        {
            var zip = Path.Combine(directory, "ota.zip");
            using (var archive = ZipFile.Open(zip, ZipArchiveMode.Create))
            {
                CreateEntry(archive, "boot.img", [0x01]);
                CreateEntry(archive, "system.img", [0x02]);
            }

            var session = new DeviceSessionViewModel();
            session.ApplyDevice(new DeviceSnapshot(
                DeviceConnectionState.FastbootConnected, "FB123", "fastboot 已连接", "vivo"));
            var logs = new OperationLogService();
            var fake = new FakeFastbootCliRunner
            {
                GetVarHandler = variable => variable.StartsWith("has-slot:", StringComparison.OrdinalIgnoreCase) ? "yes" : string.Empty
            };
            var viewModel = CreateViewModel(session, new FlashApi(), logs, fake);
            viewModel.SlotMode = SafeFlashSlotMode.BothSlots;
            var extractor = new FirmwarePartitionExtractor(payloadDumper: null);
            var partitions = await extractor.ListPartitionsAsync(zip, CancellationToken.None);
            viewModel.SetPendingSourceForTesting(zip, Path.Combine(directory, "staging"), partitions);

            await viewModel.ConfirmFlashCommand.ExecuteAsync(null);

            fake.FlashRequests.Select(request => request.Partition)
                .Should().BeEquivalentTo(["boot_a", "boot_b", "system_a", "system_b"]);
            fake.SetActiveSlots.Should().BeEmpty();
        }
        finally
        {
            Directory.Delete(directory, recursive: true);
        }
    }

    [Fact]
    public async Task ConfirmFlashAsync_non_ab_device_degrades_to_plain_flash()
    {
        var directory = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(directory);
        try
        {
            var zip = Path.Combine(directory, "ota.zip");
            using (var archive = ZipFile.Open(zip, ZipArchiveMode.Create))
            {
                CreateEntry(archive, "boot.img", [0x01]);
            }

            var session = new DeviceSessionViewModel();
            session.ApplyDevice(new DeviceSnapshot(
                DeviceConnectionState.FastbootConnected, "FB123", "fastboot 已连接", "vivo"));
            var logs = new OperationLogService();
            var fake = new FakeFastbootCliRunner(); // GetVar 默认返回空 → current-slot/has-slot 读不到
            var viewModel = CreateViewModel(session, new FlashApi(), logs, fake);
            viewModel.SlotMode = SafeFlashSlotMode.OtherSlot;
            var extractor = new FirmwarePartitionExtractor(payloadDumper: null);
            var partitions = await extractor.ListPartitionsAsync(zip, CancellationToken.None);
            viewModel.SetPendingSourceForTesting(zip, Path.Combine(directory, "staging"), partitions);

            await viewModel.ConfirmFlashCommand.ExecuteAsync(null);

            fake.FlashRequests.Select(request => request.Partition).Should().BeEquivalentTo(["boot"]);
            fake.SetActiveSlots.Should().BeEmpty();
        }
        finally
        {
            Directory.Delete(directory, recursive: true);
        }
    }

    [Fact]
    public async Task ConfirmFlashAsync_wipe_data_flashes_misc_last()
    {
        var directory = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(directory);
        try
        {
            var zip = Path.Combine(directory, "ota.zip");
            using (var archive = ZipFile.Open(zip, ZipArchiveMode.Create))
            {
                CreateEntry(archive, "boot.img", [0x01]);
            }

            var session = new DeviceSessionViewModel();
            session.ApplyDevice(new DeviceSnapshot(
                DeviceConnectionState.FastbootConnected, "FB123", "fastboot 已连接", "vivo"));
            var logs = new OperationLogService();
            var fake = new FakeFastbootCliRunner();
            var viewModel = CreateViewModel(session, new FlashApi(), logs, fake);
            viewModel.IsWipeData = true;
            var extractor = new FirmwarePartitionExtractor(payloadDumper: null);
            var partitions = await extractor.ListPartitionsAsync(zip, CancellationToken.None);
            var staging = Path.Combine(directory, "staging");
            viewModel.SetPendingSourceForTesting(zip, staging, partitions);

            await viewModel.ConfirmFlashCommand.ExecuteAsync(null);

            fake.FlashRequests.Select(request => request.Partition).Should().BeEquivalentTo(["boot", "misc"]);
            fake.FlashRequests.Last().ImagePath.Should().EndWith("wipe-data.img");
            fake.Rebooted.Should().Contain("FB123");
            logs.Entries.Should().Contain(entry => entry.Message.Contains("数据清除完成"));
        }
        finally
        {
            Directory.Delete(directory, recursive: true);
        }
    }
```

- [ ] **Step 7: 全量跑测试**

Run: `dotnet test tests/VivoKsu.App.Tests/VivoKsu.App.Tests.csproj -c Debug`
Expected: 通过(新增 4 用例全绿,0 失败)。若 `both_slots` 用例失败,核对 `ComputeTargets` 对 `system` 也返回两目标(是);若 `wipe_data` 失败,核对 `EndsWith("wipe-data.img")`(路径为 staging/extract/<guid>/wipe-data.img)。

- [ ] **Step 8: 提交**

```bash
git add src/VivoKsu.App/ViewModels/SafeFlashViewModel.cs tests/VivoKsu.App.Tests/SafeFlashViewModelTests.cs
git commit -m "feat(线刷): 刷写循环支持槽位目标(当前/对槽/双槽)+ 清除数据写 misc + 日志分区名模糊化

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

---

### Task 5: MainWindow.xaml——刷写选项 UI + 当前分区显示

**Files:**
- Modify: `src/VivoKsu.App/MainWindow.xaml`
- Verify: `src/VivoKsu.App/bin/Debug/net8.0-windows/VivoKsu.App.exe`(启动冒烟)

**Interfaces:**
- Consumes: Task 3 的 VM 属性(`SafeFlash.IsWipeData` / `SafeFlash.IsSafeFlash` / `SafeFlash.IsKeepRoot` / `SafeFlash.IsSlotCurrent` / `SafeFlash.IsSlotOther` / `SafeFlash.IsSlotBoth` / `SafeFlash.IsOptionsEnabled`)。Task 4 的 `SafeFlash.CurrentPartition` 已输出 `分区 i/n`。

- [ ] **Step 1: 面板加一行刷写选项**

`src/VivoKsu.App/MainWindow.xaml` 「VIVO 线刷」面板(现 1070-1116 行)的 `<Grid.RowDefinitions>` 由 4 行改为 5 行(Auto×5 顺序:头部 / 说明 / **选项** / 确认 / 状态):

在 `RowDefinition Height="Auto"`(说明行)后、确认面板行前插入一个 `Auto`。即:确认面板 `<Border Grid.Row="2">` → `Grid.Row="3"`,状态 `<StackPanel Grid.Row="3">` → `Grid.Row="4"`。

新选项行插入说明 TextBlock 之后、确认 Border 之前:

```xml
            <Border Grid.Row="2" Margin="24,12,24,0" Background="#F6FAFA" BorderBrush="{StaticResource EdgeBrush}" BorderThickness="1" CornerRadius="8" Padding="14,10" IsEnabled="{Binding SafeFlash.IsOptionsEnabled}">
              <Grid>
                <Grid.ColumnDefinitions>
                  <ColumnDefinition Width="Auto"/>
                  <ColumnDefinition Width="Auto"/>
                  <ColumnDefinition Width="*"/>
                  <ColumnDefinition Width="Auto"/>
                </Grid.ColumnDefinitions>
                <StackPanel Orientation="Horizontal" VerticalAlignment="Center">
                  <TextBlock Text="刷写选项" FontSize="11" FontWeight="SemiBold" Foreground="{StaticResource MutedBrush}" VerticalAlignment="Center" Margin="0,0,12,0"/>
                  <CheckBox Content="清除数据" IsChecked="{Binding SafeFlash.IsWipeData, Mode=TwoWay}" FontSize="11" VerticalAlignment="Center"/>
                  <CheckBox Content="安全刷写" IsChecked="{Binding SafeFlash.IsSafeFlash, Mode=TwoWay}" FontSize="11" VerticalAlignment="Center" Margin="14,0,0,0"/>
                  <CheckBox Content="保留ROOT" IsChecked="{Binding SafeFlash.IsKeepRoot, Mode=TwoWay}" FontSize="11" VerticalAlignment="Center" Margin="14,0,0,0"/>
                </StackPanel>
                <StackPanel Grid.Column="1" Orientation="Horizontal" VerticalAlignment="Center" Margin="24,0,0,0">
                  <TextBlock Text="槽位" FontSize="11" FontWeight="SemiBold" Foreground="{StaticResource MutedBrush}" VerticalAlignment="Center" Margin="0,0,8,0"/>
                  <RadioButton Content="当前槽" GroupName="SafeFlashSlot" IsChecked="{Binding SafeFlash.IsSlotCurrent, Mode=TwoWay}" FontSize="11" VerticalAlignment="Center"/>
                  <RadioButton Content="对槽" GroupName="SafeFlashSlot" IsChecked="{Binding SafeFlash.IsSlotOther, Mode=TwoWay}" FontSize="11" VerticalAlignment="Center" Margin="10,0,0,0"/>
                  <RadioButton Content="双槽" GroupName="SafeFlashSlot" IsChecked="{Binding SafeFlash.IsSlotBoth, Mode=TwoWay}" FontSize="11" VerticalAlignment="Center" Margin="10,0,0,0"/>
                </StackPanel>
                <Button Grid.Column="3" Content="回锁BL" Style="{StaticResource ToolButtonStyle}" IsEnabled="False" ToolTip="暂未开放" Margin="14,0,0,0" Padding="12,5" FontSize="11"/>
              </Grid>
            </Border>
```

- [ ] **Step 2: 当前分区标签文案微调(可选)**

页面底部「当前分区:」标签(现 1110-1111 行)绑定 `SafeFlash.CurrentPartition` 已显示 `分区 i/n`,标签本身 `当前分区:` 保留即可,无需改 XAML。

- [ ] **Step 3: 编译**

Run: `dotnet build src/VivoKsu.App/VivoKsu.App.csproj -c Debug`
Expected: 0 警告 0 错误。若有绑定/资源错误,核对 `SafeFlash` DataContext 路径与 VM 属性名大小写。

- [ ] **Step 4: 启动冒烟验证**

```bash
cd "src/VivoKsu.App/bin/Debug/net8.0-windows" && ./VivoKsu.App.exe & sleep 7; tasklist | grep -i "VivoKsu.App" && echo ALIVE || echo CRASHED
```
Expected: ALIVE(登录窗正常渲染,无 XAML 崩溃)。随后 `taskkill //F //IM VivoKsu.App.exe`。

- [ ] **Step 5: 全量测试**

Run: `dotnet test tests/VivoKsu.App.Tests/VivoKsu.App.Tests.csproj -c Debug`
Expected: 全绿,0 失败。

- [ ] **Step 6: 提交**

```bash
git add src/VivoKsu.App/MainWindow.xaml
git commit -m "feat(线刷): 页面新增刷写选项行(清除数据/安全刷写/保留ROOT + 槽位单选 + 回锁BL 预留)

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

---

### Task 6: 收尾——全量验证 + 发布重建

**Files:** 无源码改动(仅验证与发布)。

- [ ] **Step 1: 全量测试**

Run: `dotnet test tests/VivoKsu.App.Tests/VivoKsu.App.Tests.csproj -c Debug`
Expected: 通过,0 失败。

- [ ] **Step 2: 全量构建(Release)**

Run: `dotnet build src/VivoKsu.App/VivoKsu.App.csproj -c Release`
Expected: 0 警告 0 错误。

- [ ] **Step 3: 发布重建**

先确认没有 VivoKsu 进程占用发布目录:`tasklist | grep -i vivoksu`(有则请用户关闭)。然后:

```bash
export PATH="/c/Program Files (x86)/Microsoft Visual Studio/Installer:$PATH"
powershell.exe -ExecutionPolicy Bypass -File scripts/Publish-Release.ps1
```
Expected:`VivoKsu-win-x64.zip` 重建成功,包含内嵌 wipe-data 资源的 `VivoKsu.App.dll`。

- [ ] **Step 4: 报告**

向用户报告:6 个任务完成、全部测试全绿、发布包已重建;列出新增选项与模糊化文案。等待用户真机验证(槽位/清除数据属真实设备行为,单元测试仅覆盖编排)。
