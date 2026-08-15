# VIVO 线刷刷写选项 —— 设计

> 2026-08-15。给「VIVO 线刷」加一组刷写选项:清除数据 / 安全刷写 / 保留ROOT / 槽位(当前·对槽·双槽) / 回锁BL(预留)。**VIVO 线刷是独家功能,所有用户可见文本与日志保持模糊,不暴露技术机制。**

## 1. 目标与范围

### 目标

在 `SafeFlashViewModel` 与「VIVO 线刷」页加入刷写选项:

1. **清除数据** — 刷写收尾时把内置的 `wipe-data.img` 写入 misc 分区(触发开机数据清除)。
2. **安全刷写** — 勾选 = 跳过 lk / preloader;取消 = 连引导加载器一起刷(危险)。
3. **保留ROOT** — 跳过 boot / init_boot / vendor_boot 三个分区(保住现有 ROOT 内核)。
4. **槽位**(三选一)— 刷入当前槽 / 对槽 / 双槽。
5. **回锁BL** — 仅 UI 占位按钮(禁用 + ToolTip),本轮不实现逻辑。

**日志与文案原则**:VIVO 线刷是独家功能。所有用户可见文本(页面文案 / 确认弹窗 / 操作日志 / 状态栏)不得出现分区名、preloader/lk、misc、wipe-data、set_active、槽位名等技术细节。功能行为不变,只是表述模糊化。

### 非目标

- 回锁BL 的真实逻辑(仅预留按钮)。
- 块式分区(.new.dat)刷写支持(现有警告逻辑保留)。
- 其它页面(快速刷写 / 可视刷写)不受影响。

## 2. 选项模型

`SafeFlashViewModel` 新增 `[ObservableProperty]`:

| 属性 | 类型 | 默认 | 说明 |
| --- | --- | --- | --- |
| `IsWipeData` | bool | false | 清除数据 |
| `IsSafeFlash` | bool | **true** | 安全刷写(跳过 lk/preloader) |
| `IsKeepRoot` | bool | false | 保留ROOT(跳过 boot/init_boot/vendor_boot) |
| `SlotMode` | `SafeFlashSlotMode` | `CurrentSlot` | 槽位选择 |
| `CanRelockBl` | bool(只读) | false | 回锁BL 禁用依据 |

`SafeFlashSlotMode` 枚举放 `Models`:`CurrentSlot` / `OtherSlot` / `BothSlots`。

槽位单选在 VM 暴露三个布尔(IsSlotCurrent/IsSlotOther/IsSlotBoth),setter 统一写入 `SlotMode`;`SlotMode` 变化时通知三者刷新(`OnSlotModeChanged`)。选项在 `IsBusy || IsConfirmVisible` 时禁用(绑定 IsEnabled)。

## 3. 分区过滤

### 架构决策:提取器变「纯目录」

`FirmwarePartitionExtractor.ListPartitionsAsync` 移除内置的 lk/preloader 过滤(它只被 SafeFlashViewModel 使用,见 `AppComposition.cs:105` 与 `MainViewModel.cs:114`),返回**全量**分区(目录源 / 直接镜像 zip / payload 三条路径都去掉 `ShouldSkip` 过滤)。`ShouldSkip(string)` 静态谓词保留(判断「是否引导加载器分区」),只是不再由提取器默认套用。

`SafeFlashViewModel` 在 `PrepareFlashAsync` 拿到全量列表后按选项过滤:

```
包含(partition) =
     !(IsSafeFlash && FirmwarePartitionExtractor.ShouldSkip(name))          // 安全刷写 → 排除 lk/preloader
  && !(IsKeepRoot   && IsBootPartition(name))                               // 保留ROOT → 排除 boot/init_boot/vendor_boot
```

`IsBootPartition(name)`:精确匹配(忽略大小写)`boot` / `init_boot` / `vendor_boot`,纯函数放 VM 或小助手,便于单测。

- 「安全刷写」取消 → lk/preloader 重新进入刷写列表(确认时红字警告危险)。
- `ExtractPartitionAsync` 只按分区名解出,VM 只请求过滤后的分区,无需改动。
- `HasBlockBasedContent` 不受影响。

## 4. 槽位逻辑

### 纯函数 `SafeFlashSlotPlanner`(新文件,Models 或 Services)

```csharp
static string[] ComputeTargets(string partitionName, SafeFlashSlotMode mode, string? currentSlot, bool hasSlot)
// CurrentSlot            → [partitionName]
// OtherSlot + hasSlot    → [partitionName + "_" + OtherSlot(currentSlot)]   currentSlot 空则回退 [partitionName]
// OtherSlot + !hasSlot   → [partitionName]
// BothSlots + hasSlot    → [partitionName + "_a", partitionName + "_b"]
// BothSlots + !hasSlot   → [partitionName]

static string? OtherSlot(string? currentSlot)  // "a"↔"b",其它/null → null
static bool IsSlotBasedMode(mode)              // mode != CurrentSlot
```

### 刷写循环集成(`ConfirmFlashAsync`)

当前槽模式(默认):完全走现有路径,零额外设备查询(`has-slot` / `current-slot` 都不查)。

对槽 / 双槽模式,每分区:

1. `has-slot:<name>`(GetVarAsync,短超时)。
2. `ComputeTargets(...)` 得到目标名数组。
3. 对每个目标 `partition-type:<目标>` 探存在性(沿用 `PartitionExistsAsync`),缺失跳过。
4. `fastboot flash <目标> <镜像>`(同一镜像,多目标重复刷)。

刷完全部分区后:

- `OtherSlot` 模式:`fastboot set_active <对槽>`,再重启。
- `BothSlots` / `CurrentSlot`:不切槽。

**非 A/B 设备安全降级**:`current-slot` 读不到 / `has-slot` 全 false 时,`ComputeTargets` 回退为原样刷写,不误报、不砖机。`current-slot` 在确认阶段(ADB 态)不预读,只在对槽/双槽刷写循环里于 fastbootd 态查询。

## 5. 清除数据

- `wipe-data.img`(512KB)从 `C:\Users\17254\Downloads\wipe-data.img` 拷入 `src/VivoKsu.App/Assets/`,csproj 加 `<Resource Include="Assets\wipe-data.img" />` **嵌入程序集**(与 logo.jpg 同方式,release 无独立文件)。
- 刷写开始前(进入 fastbootd 前)把嵌入资源解到 staging 临时文件(如 `extractDirectory/wipe-data.img`),尽早失败。
- 时序(在 `ConfirmFlashAsync` 内,所有分区刷完之后、重启之前):
  1. 刷全部分区(含槽位处理)
  2. `OtherSlot` 模式 → `set_active` 切槽
  3. `IsWipeData` → `fastboot flash misc <临时 wipe-data.img>`
  4. `fastboot reboot`

## 6. 回锁BL

UI 占位按钮,`IsEnabled=false`,ToolTip「暂未开放」。不绑定命令。VM 暴露只读 `CanRelockBl => false` 或 XAML 硬编码禁用。

## 7. 日志与文案模糊化

### 页面文案

- 副标题(已模糊):`查询设备 → 下载 OTA → 解压解包 → 自动重启 fastbootd → 逐个刷入分区`(维持现状,无 preloader/lk)。
- 说明文字:维持现状(无 preloader/lk 字样)。

### 确认弹窗(`ConfirmSummary` 追加行,按启用的选项)

| 选项 | 追加文案 |
| --- | --- |
| 清除数据 | `完成后将清除设备数据。` |
| 保留ROOT | `将保留现有启动状态。` |
| 槽位=对槽 | `完成后将从另一侧启动。` |
| 槽位=双槽 | `将写入设备两侧。` |
| 安全刷写关闭 | `未启用安全模式,将完整写入固件,存在风险。`(红字警告) |

基础行维持:`将刷入 N 个分区,随后重启到 fastbootd 逐个刷写。`(不含分区名)

### 操作日志

| 现在 | 改为 |
| --- | --- |
| `已选择固件 C:\...\PD2417_...ota.zip` | `已选择固件`(去路径) |
| `固件含 N 个可刷写分区` | `固件分析完成,共 N 个分区` |
| `已连接 400E81010Y00000 \| 用时 13 秒` | `已连接设备 \| 用时 13 秒` |
| `Sending 'boot' (8192 KB)` + `OKAY [..]` + `Writing 'boot' ... OKAY` + `Finished. Total time` + `Flashing boot.img...OK`(每分区 5 行) | 每分区 1 行:`分区 {i}/{n} 写入完成` |
| `设备无 boot 分区,已跳过。` | `有 1 个分区不可用,已跳过。` |
| — | 清除数据:`正在执行数据清除` / `数据清除完成`(不出现 `misc` / `wipe-data` 字样) |
| — | 切槽不记录(set_active / 槽位名不出现) |

### 状态栏 / 页面

- 「当前分区:」显示 `分区 {i}/{n}`,不再显示分区名。
- 刷写完成状态:维持 `已刷入 N 个分区`(含跳过时 `跳过 M 个设备不存在的分区` → 改为 `已跳过 M 个不可用分区`)。

## 8. 文件改动清单

| 文件 | 改动 |
| --- | --- |
| `src/VivoKsu.App/Assets/wipe-data.img` | 新增(嵌入资源) |
| `src/VivoKsu.App/VivoKsu.App.csproj` | `<Resource Include="Assets\wipe-data.img" />` |
| `src/VivoKsu.App/Models/SafeFlashSlotMode.cs` | 新增枚举 |
| `src/VivoKsu.App/Services/SafeFlashSlotPlanner.cs` | 新增纯函数(槽位目标计算) |
| `src/VivoKsu.App/Services/FirmwarePartitionExtractor.cs` | `ListPartitionsAsync` 去掉 lk/preloader 过滤(纯目录),`ShouldSkip` 保留 |
| `src/VivoKsu.App/ViewModels/SafeFlashViewModel.cs` | 选项属性、过滤谓词、槽位/清除数据刷写编排、日志模糊化 |
| `src/VivoKsu.App/MainWindow.xaml` | 线刷页新增「刷写选项」行(复选框 + 槽位单选 + 回锁BL),当前分区显示 `分区 i/n` |
| `tests/VivoKsu.App.Tests/FirmwarePartitionExtractorTests.cs` | 更新为「返回全量」断言 |
| `tests/VivoKsu.App.Tests/SafeFlashViewModelTests.cs` | 更新日志断言;新增选项/槽位/清除数据用例 |

## 9. 测试

### 新增用例

- **选项过滤**(经 `ListPartitionsAsync` + VM 过滤):
  - 安全刷写开(默认):lk/preloader 被排除
  - 安全刷写关:lk/preloader 进入刷写列表
  - 保留ROOT 开:boot/init_boot/vendor_boot 被排除
- **槽位目标矩阵**(`SafeFlashSlotPlanner.ComputeTargets`):四模式 × 有无槽 × currentSlot a/b/空,全组合断言目标名数组。
- **清除数据**:确认刷写后,刷写序列末尾包含 `misc`,且用的是 staging 里解出的 wipe-data.img;重启在 misc 之后。
- **对槽**:刷写目标含 `boot_b`(currentSlot=a),刷完调用 `set_active b`,再重启。
- **双槽**:boot 刷两次(`boot_a`/`boot_b`),非槽分区刷一次。
- **非 A/B 降级**:currentSlot 空 / hasSlot false → 目标回退原样。

### 更新用例

- 现有 `Sending 'boot'` / `Finished. Total time:` 日志断言 → 新模糊文案(每分区 1 行)。
- 提取器 `filters_preloader_lk` 用例 → 断言返回全量。
- 其余 397 用例保持全绿。

## 10. 风险与注意事项

- **安全刷写默认开**:危险路径(刷 lk/preloader)必须显式取消勾选,确认时红字警告。
- **对槽 + 保留ROOT**:对槽仅刷保留ROOT 过滤后的分区,另一侧保留旧启动内核 —— 用户自行承担。
- **fastboot 子进程命令**一律经 `FastbootCliRunner`(已有超时/进度/错误处理),新增命令只有 `getvar has-slot`、`set_active`(已有 `SetActiveAsync`)。
- **模糊化不碰行为**:所有改动只影响文本与分区筛选,不改变下载 / 解包 / 刷写 / 重启链路本身。
