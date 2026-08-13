# VivoKsu Core Framework Design

## Goal

为现有 VivoKsu WPF 工具建立可持续扩展的核心运行框架。第一阶段只处理应用编排和设备任务生命周期，不改现有页面视觉布局，也不增加新的业务功能。

第一阶段完成后，所有设备相关前台任务都具备统一的串行执行、取消、状态显示、日志关联和异常收口；ADB/Fastboot 自动检测在任务执行期间自动暂停，并在任务结束后恢复。

## Current Baseline

- 技术栈：.NET 8 WPF、CommunityToolkit.Mvvm、HandyControl。
- 设备后端：`FastbootRsBackend`，封装现有 fastboot-rs C ABI 与 ADB/Fastboot 调用。
- 已有页面：设备概览、快速刷写、ADB 投屏、ROOT、文件管理、线刷。
- 已有共享状态：`DeviceSessionViewModel`、`OperationLogService`、固定左下状态面板和右侧日志面板。
- 已有行为约束：单设备；自动轮询连续一次空结果保留原连接，连续两次才标记断开；任务期间暂停自动设备检测。
- 运行平台：Windows 10/11 x64，资源随发布包携带；不恢复 Windows 7 支持。

## Design Principles

1. 保留现有服务边界和 native backend，不让 View/ViewModel 直接调用 native API。
2. 设备写操作一次只允许一个；读取型后台轮询必须服从前台任务锁。
3. 每个任务都有明确的开始、阶段、成功、取消、失败和结束路径。
4. UI 状态只绑定可观察状态，后台线程不直接修改 WPF 集合或控件。
5. 先引入稳定的框架边界，再迁移具体业务，避免一次性重写现有功能。

## Architecture

```text
App
└── AppComposition
    ├── FastbootRsBackend
    ├── OperationLogService
    ├── DeviceSessionViewModel
    ├── OperationCoordinator
    ├── DeviceMonitorService
    ├── DeviceSessionService / DeviceInfoService
    └── feature ViewModels and Services

MainWindow
└── MainViewModel
    ├── DeviceSession
    ├── CoordinatorState
    ├── MonitorState
    ├── feature ViewModels
    └── OperationLogViewModel
```

### AppComposition

新增 `AppComposition`（或同等组合根类型）负责构造应用依赖。`MainWindow` 只接收已经构造好的 `MainViewModel`，不再手动 new backend、日志服务和页面服务。

组合根负责：

- 创建唯一的 `FastbootRsBackend` 和 `OperationLogService`。
- 创建唯一的 `DeviceSessionViewModel` 和 `OperationCoordinator`。
- 将协调器注入设备监视器、设备会话服务和需要占用设备的功能服务。
- 复用同一个 `QuickFlashService` 实例，避免 ROOT 与快速刷写拥有不同的执行状态。
- 在应用关闭时停止监视器、取消活动任务并释放进程型资源。

### OperationCoordinator

`OperationCoordinator` 是所有前台设备任务的单一入口，使用 `SemaphoreSlim(1, 1)` 实现单设备串行执行。

建议公开接口：

```csharp
public interface IOperationCoordinator
{
    bool IsBusy { get; }
    OperationStateSnapshot State { get; }
    event EventHandler? StateChanged;
    Task RunAsync(
        OperationKind kind,
        string title,
        Func<OperationContext, CancellationToken, Task> operation,
        CancellationToken cancellationToken = default);
    void CancelCurrent();
}
```

`OperationContext` 提供：

- 操作关联 ID。
- `ReportStage(string text)`，更新左下角状态和日志阶段。
- `ReportProgress(double? value)`，为后续文件传输和刷写进度保留接口。
- `ThrowIfCancellationRequested()`。

`OperationStateSnapshot` 至少包含：

- `OperationKind Kind`。
- `string OperationId`。
- `string Title`。
- `string Stage`。
- `double? Progress`。
- `DateTimeOffset StartedAt`。
- `bool IsCancellable`。

协调器行为：

1. 等待单任务锁；获取锁后设置 `IsBusy=true`，调用 `DeviceSessionViewModel.BeginOperation`，写入带关联 ID 的 Info 日志。
2. 执行委托；阶段变化只通过 `OperationContext` 发布。
3. 正常完成时写 Success 日志并调用 `CompleteOperation`。
4. 收到取消时写 Warning 日志并调用 `CancelOperation`。
5. 其他异常写 Error 日志并调用 `FailOperation`，异常继续抛给调用方决定是否显示页面级错误。
6. `finally` 清理状态、释放锁并触发 `StateChanged`。

协调器不负责具体 ADB/Fastboot 命令，也不持有页面状态；它只负责执行边界和生命周期。

### DeviceMonitorService

新增 `DeviceMonitorService`，替代 `MainWindow` 中的 `DispatcherTimer` 业务逻辑。它使用 `PeriodicTimer` 或等价异步循环，默认每 3 秒执行一次自动刷新。

规则：

- 调用 `DeviceSessionService.RefreshAsync(..., DeviceRefreshMode.Automatic)` 前检查 `IOperationCoordinator.IsBusy`。
- 协调器忙碌时跳过本轮，不改变现有连接状态。
- 前台任务结束后立即触发一次补偿刷新，不等待完整轮询周期。
- 监视器生命周期由 `StartAsync`/`StopAsync` 控制，停止时取消循环并等待当前刷新结束。
- 手动刷新仍由 `MainViewModel.RefreshDeviceCommand` 触发，但与自动刷新共享同一刷新闸门，禁止并发探测。

### Feature migration boundary

现有功能按以下顺序迁移到协调器：

1. 设备概览重启：普通重启、Bootloader、Fastboot。
2. 快速刷写：等待目标模式、刷写、成功后重启。
3. ROOT 自动流程：安装管理器、修补镜像、重启、刷写、最终重启。
4. 文件管理和 APK 安装：上传、下载、删除、安装。
5. 线刷分区读取和投屏控制。

迁移期间，未迁移的服务保留现有行为，但不能绕过协调器修改设备状态。每迁移一个模块都增加对应的协调器测试和回归测试。

## State and Data Flow

```text
DeviceMonitorService ──自动刷新──> DeviceSessionService
        │                              │
        │  IsBusy=false                ├── DiscoverAsync
        │                              ├── ReadAdbAsync / ReadFastbootAsync
        │                              └── ApplyDevice / ApplyDetails
        │
Feature ViewModel ──RunAsync────────> OperationCoordinator
                                      ├── DeviceSession status
                                      ├── OperationLogService
                                      └── feature service command sequence
```

设备发现结果、前台任务状态和日志是三条独立数据流：

- 发现流更新连接和设备详情。
- 操作流更新当前任务状态和左下角设备状态。
- 日志流保留完整阶段、命令结果和错误信息。

任何前台任务都不得通过重新发现设备来推断成功；任务结果以命令返回和明确的后续状态刷新为准。

## Error, Cancellation, and Recovery

- 所有设备命令使用传入的 `CancellationToken`，不使用无法取消的无限等待。
- 等待 ADB/Fastboot 目标时支持取消；取消后不发送额外重启。
- 刷写/ROOT 失败时保留已生成的镜像和日志，状态进入 Failed，允许用户手动重新检测和继续。
- 自动检测异常只写 Warning/Error 日志，不覆盖正在显示的前台任务状态。
- 投屏进程退出事件必须回到创建它的同步上下文；自动重启失败只记录错误，不让异常离开事件回调线程。
- 应用关闭时按“停止监视器 -> 取消协调器任务 -> 停止投屏进程 -> 释放 native/backend 资源”的顺序收口。

## Testing Strategy

新增测试分四层：

### Coordinator tests

- 两个并发任务始终串行执行。
- 第二个任务在第一个任务完成后才能开始。
- `CancelCurrent` 触发取消令牌并进入 Canceled。
- 正常、取消、异常三种路径都恢复 `IsBusy=false`。
- 每种路径都写入正确级别的关联日志。

### Monitor tests

- 协调器忙碌时不调用设备发现。
- 任务结束后触发补偿刷新。
- 手动刷新与自动刷新不会并发。
- 监视器停止后不再产生刷新调用。

### Composition tests

- 所有页面 ViewModel 共享同一个 backend、日志服务、session 和 coordinator。
- ROOT 与快速刷写复用同一个刷写服务/协调器。
- 组合根可以在缺少 native DLL 的测试环境中构造替代 API。

### Regression tests

- 运行现有全量测试，保留现有设备断开延迟、fastbootd 判定、镜像扩展名、投屏异常退出恢复等行为。
- Release 构建后确认发布包仍包含 `platform-tools`、`scrcpy`、ROOT 资源和 APK。

## Delivery Order

1. 新增 `OperationCoordinator`、状态模型和单元测试。
2. 新增 `DeviceMonitorService`，把自动轮询从 `MainWindow` 移出。
3. 新增 `AppComposition`，重构应用启动和关闭生命周期。
4. 迁移概览重启与快速刷写。
5. 迁移 ROOT、文件管理、APK 安装、线刷和投屏。
6. 运行全量测试、Release 构建和发布包资源检查。

## Scope and Non-goals

本阶段不改变 UI 视觉结构，不引入新的 UI 库，不支持多设备并行，不恢复 Windows 7，不增加任意分区刷写，也不重新设计已有 ROOT 算法和 Vivo vendor_boot 处理流程。

## Implementation Note

2026-08-11: OperationCoordinator, DeviceMonitorService, AppComposition, reboot migration, and quick-flash migration are implemented and covered by the framework test suite. ROOT, file management, line-flash extraction, and ADB mirror still use their existing task entry points and are the next migration increment.
