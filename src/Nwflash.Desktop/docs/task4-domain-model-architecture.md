# Task4 领域层架构说明（nwflash-domain）

本次仅迁移 `nwflash-domain`，保留纯领域模型与策略，不接入 Tauri/IO/网络/Windows API。

## 目录结构

- `src/app_page.rs`
  - 迁移 `Models/AppPage.cs`，保留页面枚举。
- `src/device.rs`
  - 迁移 `DeviceConnectionState`, `DeviceSnapshot`, `DeviceDetailsSnapshot`, `DeviceFileEntry`, `DeviceRefreshMode`。
  - 实现 `parse_fastboot_rs_output`（对应旧 `FastbootRsDeviceParser.Parse`）。
- `src/operation.rs`
  - 迁移 `OperationKind`, `OperationStateSnapshot`, `OperationLogLevel`, `OperationLogEntry`, `UsageLogEntry`。
- `src/partition.rs`
  - 迁移 `PartitionTransportKind`, `PartitionOperationKind`, `PartitionTaskState`, `DevicePartition`, `PartitionSnapshot`, `PartitionTask`, `PartitionExecutionPlan`, `PartitionTransferProgress`。
  - 迁移 `PartitionRiskPolicy`（`is_high_risk_partition`）与 `PartitionExecutionPlanBuilder`（`build_write/build_backup/build_erase`）。
  - 增加 `format_partition_size` 以对齐旧的分区大小显示逻辑。
- `src/quick_flash.rs`
  - 迁移 `QuickFlashPartition`, `FlashImageInfo`, `QuickFlashRequest`, `QuickFlashOptions`, `FastbootTarget`。
  - 增加 `build_quick_flash_plan`（纯函数），复现快闪双槽映射与前置检查。
- `src/safe_flash.rs`
  - 迁移 `SafeFlashSlotMode`，实现 `is_slot_based_mode/other_slot/compute_targets`。
  - 增加 `should_skip_safe_flash_partition`（`lk` 与 `preloader*` 过滤口径）。
- `src/firmware.rs`
  - 迁移 `RomInfo`, `FirmwarePackageInspection`, `PayloadPartitionEntry`, `PayloadExtractionResult`。
- `src/download.rs`
  - 迁移 `DownloadProgress`。
- `src/log.rs`
  - 定义日志相关 `LogLevel` 与 `LogEntry`（供后续统一日志语义使用）。
- `src/error.rs`
  - 新增领域错误分类：用户取消、设备不可用、授权拒绝、远端 API、外部工具、文件格式、参数、非法操作、内部错误。

## 对应测试（本轮）

- [tests/partition_policy.rs](../../src-tauri/crates/nwflash-domain/tests/partition_policy.rs)
  - 高风险分区判定
  - 大小格式化
  - 分区执行计划构建与 ADB Root 校验（不存在/空/sparse/容量）
  - 设备连接状态解析
- [tests/safe_flash_slots.rs](../../src-tauri/crates/nwflash-domain/tests/safe_flash_slots.rs)
  - 槽位模式与映射
- [tests/quick_flash_plan.rs](../../src-tauri/crates/nwflash-domain/tests/quick_flash_plan.rs)
  - 快闪批量计划与双槽预检
- [tests/firmware_models.rs](../../src-tauri/crates/nwflash-domain/tests/firmware_models.rs)
  - 固件托管分区筛选、大小文本格式
- [tests/operation_log.rs](../../src-tauri/crates/nwflash-domain/tests/operation_log.rs)
  - 日志中文等级标签

## 本轮验收

- 命令：`cargo test -p nwflash-domain`
- 结果：全量通过（16 条测试通过）。
