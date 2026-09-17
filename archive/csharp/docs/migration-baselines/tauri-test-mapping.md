# 奶蛙Flash C# 到 Tauri 测试映射

本清单以仓库源码为准，覆盖 `tests/VivoKsu.App.Tests/*Tests.cs` 的全部 64 个测试类。`direct` 表示同一行为由一个主要 Rust/React 测试面承接；`merged` 表示原 C# 类的职责在迁移后被拆入多个受限边界，或平台专属职责由 Tauri 等效验收替代。证据列只列可执行测试文件，不以计划或实现文件代替测试证据。

映射门禁由 `src/Nwflash.Desktop/src-tauri/tests/e2e/api.rs` 执行：源目录新增、删除或重命名 `*Tests.cs` 后，本表未同步会失败；重复行、空证据、无效分类、越界证据路径或不存在的证据文件同样会失败。

| C# test class | coverage | Rust / React / native E2E evidence | source-grounded behavior |
| --- | --- | --- | --- |
| `AdbFileServiceTests.cs` | direct | `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/tests/file_manager.rs` | 远端目录解析、受限路径、pull/push/install/delete 参数边界。 |
| `AdbRootPartitionTransportTests.cs` | merged | `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/tests/partition_workspace.rs`<br>`src/Nwflash.Desktop/src-tauri/tests/e2e/device_process.rs` | ADB Root 分区发现、路径冻结、写入/擦除和参数数组。 |
| `AdbRootTransferRunnerTests.cs` | merged | `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/tests/file_transfer.rs`<br>`src/Nwflash.Desktop/src-tauri/tests/e2e/device_process.rs` | `dd` 的非 PTY/exec-out 传输、元字符拒绝及本地路径不进入设备参数。 |
| `AppCompositionTests.cs` | merged | `src/Nwflash.Desktop/src-tauri/tests/e2e/operation.rs`<br>`src/Nwflash.Desktop/src/AppSessionLifecycle.test.tsx` | AppState 共享会话/协调器、生命周期事件和退出闭环。 |
| `AppVersionControlTests.cs` | merged | `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/tests/version_contract.rs`<br>`src/Nwflash.Desktop/src/AppSessionAuthFlow.test.tsx` | 版本策略解析、网络失败降级及强更登录门禁。 |
| `DeviceInfoServiceTests.cs` | direct | `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/tests/device_info.rs` | ADB getprop、电量与 Fastboot 详情投射。 |
| `DeviceMonitorServiceTests.cs` | direct | `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/tests/device_monitor.rs` | 忙碌跳过、补偿刷新、身份变化广播和断开防抖。 |
| `DeviceSessionServiceTests.cs` | merged | `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/tests/device_session.rs`<br>`src/Nwflash.Desktop/src-tauri/crates/nwflash-application/tests/device_monitor.rs` | ADB 优先、Fastboot 回退、详情保留及自动刷新状态。 |
| `DeviceSessionViewModelTests.cs` | merged | `src/Nwflash.Desktop/src/pages/OverviewPage.test.tsx`<br>`src/Nwflash.Desktop/src/components/AppShell.test.tsx` | 连接状态、颜色语义和壳体设备状态投影。 |
| `DotNetRuntimeDetectorTests.cs` | merged | `src/Nwflash.Desktop/src-tauri/tests/build_smoke.rs`<br>`src/Nwflash.Desktop/src/pages/SoftwarePage.test.tsx` | Tauri 不依赖 .NET Desktop Runtime；以 Rust workspace 构建和组件就绪状态替代。 |
| `EmbeddedWipeDataTests.cs` | merged | `src/Nwflash.Desktop/src-tauri/crates/nwflash-domain/tests/safe_flash_slots.rs`<br>`src/Nwflash.Desktop/src-tauri/crates/nwflash-application/tests/safe_flash.rs` | wipe-data 从 WPF 内嵌镜像写盘改为受控安全刷写清理/槽位计划，不向浏览器暴露镜像。 |
| `ExternalResourceLocationsTests.cs` | direct | `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/tests/paths.rs` | 资源根目录解析和可写性检查。 |
| `FastbootPartitionServiceTests.cs` | direct | `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/tests/partition_workspace.rs` | Fastboot 分区大小、元数据和不支持项投射。 |
| `FastbootPartitionTableParserTests.cs` | merged | `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/tests/partition_workspace.rs`<br>`src/Nwflash.Desktop/src-tauri/crates/nwflash-domain/tests/partition_policy.rs` | 分区表解析、全部行保留和高风险标注。 |
| `FastbootPartitionTransportTests.cs` | direct | `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/tests/partition_workspace.rs` | Fastboot 发现、写入、擦除和不支持备份。 |
| `FastbootRsBackendTests.cs` | merged | `src/Nwflash.Desktop/src-tauri/tests/e2e/device_process.rs`<br>`src/Nwflash.Desktop/src-tauri/crates/nwflash-application/tests/partition_workspace.rs` | native 后端命令由 PlatformTools 参数数组及分区工作区承接。 |
| `FastbootRsDeviceParserTests.cs` | direct | `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/tests/device_session.rs` | ADB/Fastboot/离线/未授权发现结果分类。 |
| `FileManagerViewModelTests.cs` | direct | `src/Nwflash.Desktop/src/pages/FileManagerPage.test.tsx` | `/sdcard` 导航、按钮门禁、对话框路径和受限命令调用。 |
| `FirmwareExtractViewModelTests.cs` | merged | `src/Nwflash.Desktop/src/pages/FirmwareExtractPage.test.tsx`<br>`src/Nwflash.Desktop/src-tauri/crates/nwflash-application/tests/firmware_extract.rs` | 不透明固件 ID、分区选择、提取取消与快速刷写交接。 |
| `FirmwarePackageExtractionServiceTests.cs` | direct | `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/tests/firmware_package.rs` | ZIP 成员提取、唯一暂存、导出和取消清理。 |
| `FirmwarePackageInspectorTests.cs` | direct | `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/tests/firmware_package.rs` | payload 检测、镜像清单排序和受管归档路径。 |
| `FirmwarePartitionExtractorTests.cs` | merged | `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/tests/firmware_extract.rs`<br>`src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/tests/payload_dumper.rs` | 格式识别、远程 Range 检查及 payload 分区结果验证。 |
| `HeartbeatServiceTests.cs` | merged | `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/tests/api_contract.rs`<br>`src/Nwflash.Desktop/src-tauri/crates/nwflash-application/tests/session_lifecycle.rs` | 心跳请求、强退/强更响应和会话停止。 |
| `LineFlashViewModelTests.cs` | merged | `src/Nwflash.Desktop/src/pages/LineFlashPage.test.tsx`<br>`src/Nwflash.Desktop/e2e-tests/specs/interactions.e2e.ts` | 分区筛选、映射、确认、执行、备份和取消交互。 |
| `MainViewModelTests.cs` | merged | `src/Nwflash.Desktop/src/components/AppShell.test.tsx`<br>`src/Nwflash.Desktop/src/app/pageManifest.test.ts` | 十项导航、全局忙态、进度优先级和退出入口。 |
| `MirrorServiceTests.cs` | merged | `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/tests/mirror.rs`<br>`src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/tests/mirror_runtime.rs` | scrcpy 参数/ADB 环境、自动协调、缺失工具和真实进程启动失败。 |
| `OfficialKernelSuResourceTests.cs` | direct | `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/tests/root_resources.rs` | 官方 KernelSU 管理器、KMI 和验证后的 libksud 资源。 |
| `OnlineViewModelTests.cs` | merged | `src/Nwflash.Desktop/src/pages/OnlineStatusPage.test.tsx`<br>`src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/tests/api_contract.rs` | 在线会话列表、当前会话标记、轮询/刷新和错误空态。 |
| `OperationCoordinatorTests.cs` | merged | `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/tests/operation_coordinator.rs`<br>`src/Nwflash.Desktop/src-tauri/tests/e2e/operation.rs` | 单任务互斥、授权、进度、失败/取消收尾和后续恢复。 |
| `OperationLogEntryTests.cs` | direct | `src/Nwflash.Desktop/src-tauri/crates/nwflash-domain/tests/operation_log.rs` | 日志时间、级别和 DTO 序列化。 |
| `OperationLogServiceTests.cs` | direct | `src/Nwflash.Desktop/src/pages/OperationLogPage.test.tsx` | 文件日志快照、空态和容错投影。 |
| `OperationLogViewModelTests.cs` | direct | `src/Nwflash.Desktop/src/pages/OperationLogPage.test.tsx` | 日志加载、空态、非法结构和错误提示。 |
| `OtaApiClientTests.cs` | direct | `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/tests/api_contract.rs` | ROM 查询编码、认证头、状态码和响应解析。 |
| `OtaDownloadServiceTests.cs` | direct | `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/tests/ota_download.rs` | OTA 目标、空间预检、流式进度、取消和暂存清理。 |
| `OverviewViewModelTests.cs` | merged | `src/Nwflash.Desktop/src/pages/OverviewPage.test.tsx`<br>`src/Nwflash.Desktop/src/AppSessionLifecycle.test.tsx` | 设备档案、固定重启命令、错误展示和设备事件刷新。 |
| `PartitionExecutionPlanBuilderTests.cs` | merged | `src/Nwflash.Desktop/src-tauri/crates/nwflash-domain/tests/partition_policy.rs`<br>`src/Nwflash.Desktop/src-tauri/crates/nwflash-application/tests/partition_workspace.rs` | 选择、映射、高风险摘要和参数化执行计划。 |
| `PartitionExecutionServiceTests.cs` | direct | `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/tests/partition_workspace.rs` | 串行执行、逐分区状态、失败停止和取消。 |
| `PartitionRiskPolicyTests.cs` | direct | `src/Nwflash.Desktop/src-tauri/crates/nwflash-domain/tests/partition_policy.rs` | 高风险分区分类与确认策略。 |
| `PartitionWorkspaceViewModelTests.cs` | merged | `src/Nwflash.Desktop/src/pages/LineFlashPage.test.tsx`<br>`src/Nwflash.Desktop/src-tauri/crates/nwflash-application/tests/partition_workspace.rs` | 工作区状态、传输枚举、选中项、确认和备份。 |
| `PayloadDumperProvisionerTests.cs` | merged | `src/Nwflash.Desktop/src-tauri/tests/e2e/api.rs`<br>`src/Nwflash.Desktop/src-tauri/tests/e2e/firmware.rs` | 下载候选/完整性/清理与固定 payload_dumper 参数在统一受控资源链验证。 |
| `PayloadDumperRunnerTests.cs` | merged | `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/tests/payload_dumper.rs`<br>`src/Nwflash.Desktop/src-tauri/crates/nwflash-application/tests/firmware_extract.rs`<br>`src/Nwflash.Desktop/src-tauri/tests/e2e/firmware.rs` | metadata/extract 参数、输出验证、取消和进程失败。 |
| `PlatformToolsNativeApiTests.cs` | merged | `src/Nwflash.Desktop/src-tauri/tests/e2e/device_process.rs`<br>`src/Nwflash.Desktop/src-tauri/crates/nwflash-application/tests/partition_workspace.rs` | ADB/Fastboot 发现、getvar/flash/erase/set-active 参数数组和失败输出。 |
| `PublishReleaseScriptTests.cs` | merged | `src/Nwflash.Desktop/src-tauri/tests/build_smoke.rs`<br>`src/Nwflash.Desktop/e2e-tests/specs/visual-baseline.e2e.ts` | 旧 WPF 发布脚本不构建 Tauri；由生产构建烟测与无测试桥生产包验收替代。 |
| `QuickFlashServiceTests.cs` | direct | `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/tests/quick_flash.rs` | 固定预置、双槽计划、串号/镜像验证和命令参数。 |
| `QuickFlashViewModelTests.cs` | merged | `src/Nwflash.Desktop/src/pages/QuickFlashPage.test.tsx`<br>`src/Nwflash.Desktop/e2e-tests/specs/interactions.e2e.ts` | 图片选择、不透明计划、显式确认、执行和取消。 |
| `RemoteAssetDownloaderTests.cs` | merged | `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/tests/resource_downloader.rs`<br>`src/Nwflash.Desktop/src-tauri/tests/e2e/api.rs` | 候选回退、超时、完整性失败、不覆盖已批准文件和暂存清理。 |
| `ResourceDownloadViewModelTests.cs` | direct | `src/Nwflash.Desktop/src/pages/ResourceDownloadPage.test.tsx` | 固定资源键、缺失选择、安装刷新与受控取消。 |
| `RootPatchArtifactServiceTests.cs` | direct | `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/tests/root_patch.rs` | 修补产物命名、大小边界、验证与发布。 |
| `RootViewModelTests.cs` | merged | `src/Nwflash.Desktop/src/pages/RootPage.test.tsx`<br>`src/Nwflash.Desktop/src-tauri/crates/nwflash-application/tests/root.rs` | 管理器枚举、KMI、受控修补、不透明工件和确认交接。 |
| `SafeFlashSlotPlannerTests.cs` | direct | `src/Nwflash.Desktop/src-tauri/crates/nwflash-domain/tests/safe_flash_slots.rs` | A/B 槽位选择、跳过和收尾计划。 |
| `SafeFlashViewModelTests.cs` | merged | `src/Nwflash.Desktop/src/pages/SafeFlashPage.test.tsx`<br>`src/Nwflash.Desktop/src-tauri/crates/nwflash-application/tests/safe_flash.rs` | 在线/本地来源、不透明会话、预检、确认、取消和块式 OTA 提示。 |
| `ScrcpyProvisioningServiceTests.cs` | merged | `src/Nwflash.Desktop/src/pages/SoftwarePage.test.tsx`<br>`src/Nwflash.Desktop/src-tauri/tests/e2e/api.rs`<br>`src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/tests/mirror_runtime.rs` | 固定组件安装入口、GitHub/下载故障保护及验证后 scrcpy 真实启动失败。 |
| `ScrcpyToolLocatorTests.cs` | merged | `src/Nwflash.Desktop/src/pages/SoftwarePage.test.tsx`<br>`src/Nwflash.Desktop/src-tauri/crates/nwflash-application/tests/mirror.rs` | 组件就绪投射、缺失工具拒绝及受控 scrcpy/ADB 路径。 |
| `ServerOperationGateTests.cs` | merged | `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/tests/api_contract.rs`<br>`src/Nwflash.Desktop/src-tauri/crates/nwflash-application/tests/operation_coordinator.rs`<br>`src/Nwflash.Desktop/src-tauri/tests/e2e/api.rs` | 服务端授权请求、拒绝/故障 fail-closed 和设备闭包不执行。 |
| `SoftwareViewModelTests.cs` | direct | `src/Nwflash.Desktop/src/pages/SoftwarePage.test.tsx` | 版本、组件/驱动状态、刷新、安装和错误降级。 |
| `SystemProcessRunnerTests.cs` | merged | `src/Nwflash.Desktop/src-tauri/tests/e2e/device_process.rs` | 参数数组、环境、退出码、超时、进程树取消和注入失败。 |
| `ToolPathPreferencesTests.cs` | merged | `src/Nwflash.Desktop/src/pages/SoftwarePage.test.tsx`<br>`src/Nwflash.Desktop/src-tauri/crates/nwflash-application/tests/mirror.rs` | 浏览器不持久化任意工具路径；Rust 组件状态与验证后的工具路径替代 WPF 偏好。 |
| `UsageLogUploaderTests.cs` | merged | `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/tests/api_contract.rs`<br>`src/Nwflash.Desktop/src-tauri/crates/nwflash-application/tests/operation_coordinator.rs` | 批量上传契约及成功/失败/取消使用状态生成。 |
| `VivoDriverDetectorTests.cs` | direct | `src/Nwflash.Desktop/src-tauri/crates/nwflash-windows/tests/driver.rs` | Windows 驱动注册表检测和状态映射。 |
| `VivoDriverInstallerTests.cs` | direct | `src/Nwflash.Desktop/src-tauri/crates/nwflash-windows/tests/driver_installer.rs` | 驱动包解析、安装参数和失败状态。 |
| `VivoFirmwareExtractorTests.cs` | direct | `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/tests/vivo_firmware.rs` | Vivo gzip/zstd 条目、流式提取、路径安全和取消。 |
| `VivoKsuDevicePatchServiceTests.cs` | merged | `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/tests/root.rs`<br>`src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/tests/root_patch.rs` | 设备修补命令、产物校验、不透明引用和失败清理。 |
| `VivoRootResourceServiceTests.cs` | direct | `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/tests/root_resources.rs` | 管理器白名单、APK/libksud 校验、KMI 和原子替换。 |
| `VivoVendorBootProcessorTests.cs` | direct | `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/tests/vendor_boot.rs` | vendor_boot 解包、模块定位、重打包和错误处理。 |

## 门禁命令

```powershell
cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --test e2e api::every_csharp_test_file_has_one_source_grounded_mapping -- --exact
```
