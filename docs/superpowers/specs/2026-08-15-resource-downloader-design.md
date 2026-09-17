# 组件安装窗(Resource Downloader)设计

日期:2026-08-15
状态:已批准(用户选定方案 A · 独立模态窗)

## Context

发布瘦身后 scrcpy / ROOT 管理器 APK×2 / payload_dumper 不再随包,首次使用时按需下载
(多镜像 failover + SHA-256 校验,落盘 `C:\nwflash`)。需要一个登录后的「组件安装」入口:
检测缺失资源、并行下载、可选 / 全装 / 跳过,软件页可随时重开补装。

## 已确认决策

1. **弹出时机**:登录后主窗显示,检测 4 个资源,只要有缺失就弹模态窗;全部就绪静默跳过。
2. **形态**:模态盖在主窗上;「跳过」= 关窗进主界面(资源仍是首次使用时按需下载兜底)。
3. **方案**:独立模态窗(液态玻璃),非软件页内联、非驱动提醒窗骨架。
4. **并发**:选中项全并行下载(用户「同时下载 payload apk 这些」),每项独立进度条。

## 资源清单与就绪判定

| 资源 | 就绪判定 | 下载入口(加进度参) |
|---|---|---|
| scrcpy 投屏 | `ScrcpyProvisioningService.IsInstalled`(新增) | `EnsureInstalledAsync(ct, progress)` |
| KSU 管理器 APK | `VivoRootResourceService.IsManagerApkInstalled("KSU")`(新增) | `EnsureManagerApkAsync(manager, ct, progress)` |
| KernelSU 管理器 APK | `IsManagerApkInstalled("OfficialKsu")` | 同上 |
| payload_dumper | `PayloadDumperProvisioner.IsAvailable`(已有) | `EnsureInstalledAsync(ct, progress)` |

就绪 = 落盘文件存在(`ExternalResourceLocations`:固定 `C:\nwflash`,不可写回退 `%LOCALAPPDATA%\VivoKsu`)。

## 复用(不重造轮子)

- **下载**:`RemoteAssetDownloader`(直连→gh-proxy→ghfast→ghproxy 多镜像 failover +
  SHA-256/长度校验 + staging 原子落盘)——已有,只把进度类型 `IProgress<long>` 升级为
  `IProgress<DownloadProgress>`(字节 + 总字节,总字节取响应 `Content-Length`)。
- **位置**:`ExternalResourceLocations`。**哈希**:APK 走 `ManagerApkSha256`,
  payload 走 `PayloadDumperSha256`,scrcpy 解压自校验。
- **主题 / 控件**:`App.xaml` 全部样式(teal `#087A70`、圆角、状态芯片、`ModernCheckBoxStyle`、
  按钮 Primary/Soft/Ghost、Cascadia Mono 状态字)。
- **玻璃手法**:`LoginWindow` 的 `WindowStyle=None + AllowsTransparency + DropShadow` 圆角卡片;
  本窗在此基础上做「液态玻璃 sheet」= 半透明白+teal 渐变 + 1px 高光边 + 内顶高光 + 两层模糊色斑垫底。

## 架构

```
ResourceDownloadViewModel (可测,不依赖 Window)
 ├─ Detect():用各服务就绪判定构建 4 个 ResourceDownloadItemViewModel
 ├─ HasMissing / SelectedCount / IsDownloading / DoneCountText
 ├─ InstallSelectedAsync():选中的缺失项 Task.WhenAll 并行下载
 │    ├─ 每项 Status: 待下载 → 下载中 → 已就绪 / 失败 / 跳过(取消)
 │    └─ 失败隔离:单项失败标 Failed,其余继续;全结束窗口汇总
 ├─ CancelAsync():取消在途下载(用户点「取消」)
 └─ 进度:DownloadProgress(字节, 总字节?) → 每项 Progress 0-1 + 字节文本;总字节未知用不确定条

ResourceDownloadWindow (XAML, 液态玻璃)
 ├─ 顶栏:标题「组件安装」+ 副题「奶蛙Flash 依赖组件就绪 · 缺失 N 项」+ ✕
 ├─ 中部:4 行(ModernCheckBox + 名称 + 大小 + 状态芯片 + 细进度条)
 │    └─ 已就绪项:勾选禁用(显示 ✓ 已就绪),只对缺失项可勾
 └─ 底部:计数「N / M 下载中」+ 跳过(ghost)+ 主按钮(teal primary;全勾时「全部安装」,部分勾选时「下载所选 (N)」;下载中变 取消)
```

单文件职责:`ResourceDownloadItemViewModel`(单项状态)+ `ResourceDownloadViewModel`(编排)+
`ResourceDownloadWindow`(纯 UI)。

## 接线

- **`AppComposition`**:提取 `scrcpyProvisioner` / `payloadDumperProvisioner` / `rootResources`
  为字段,暴露 `CreateResourceDownloadViewModel()`。
- **`App.xaml.cs` RunApplicationLoop**:主窗显示后 → `ShowResourceDownloaderIfNeeded()`
  (检测缺失,有则 `new ResourceDownloadWindow(vm).ShowDialog()`)→ 再 `CheckAndRemindDriverAsync()`
  (驱动提醒照旧,避免双模态窗叠放)。
- **`SoftwareViewModel`**:新增 `openResourceDownloader` 回调 ctor 参(同 `onReinstallDriver`
  模式)+ `OpenResourceDownloaderCommand`(软件页头部「重新检测」旁加「安装组件」按钮),
  关窗后 `RefreshAsync()` 刷新状态。

## 测试

- 新增 `ResourceDownloadViewModelTests`:检测(有/无缺失)、勾选、全装并行、跳过、
  进度映射、单项失败隔离、取消。
- 新增/适配:scrcpy 反射测试调 `EnsureInstalledAsync` 需补 progress 参数;
  `RemoteAssetDownloaderTests` 进度类型适配(现有传 null 不受影响)。
- 全量测试绿 + 构建 0 警告;UI 走 verify 脚本截图验证液态玻璃窗。

## 风险与兜底

| 风险 | 兜底 |
|---|---|
| 液态玻璃在 WPF 无 backdrop-filter,毛玻璃是「窗内色斑模糊」近似 | 半透明白 sheet + BlurEffect 色斑 + 高光边;纯白底 fallback 也成立 |
| 用户开多个窗(登录自动弹 + 软件页手动开) | 下载器内信号量防重入;软件页按钮忙时禁用 |
| 单项失败用户无感 | 失败项红色芯片 + 窗内汇总行「N 项失败,可稍后在软件页重试」 |
