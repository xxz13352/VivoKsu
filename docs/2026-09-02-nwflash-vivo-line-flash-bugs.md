# Nwflash Vivo 线刷严重 BUG 待完成清单

状态：**待完成**

> **重要约束：以下问题未逐项修复。今后任何代理或开发者要修改与这些问题相关的代码，必须先向用户说明变更内容、影响范围、验证方式与回退方案，并获得用户明确批准后才可动手。**

## 审查范围

- 对照归档 C# / WPF 源码：`archive/csharp/src/VivoKsu.App/`
- 审查 Rust / Tauri 源码：`src/Nwflash.Desktop/src-tauri/`
- 重点覆盖 Vivo 固件提取、受控分区、快速刷写、分区工作区、Fastboot/ADB 执行链路。
- 本文档只记录结论和证据，不引入代码修改。

## 高风险 BUG

### 1. 执行前重定向设备序列号，绕过跨设备安全校验

- **C# 参考行为**：执行计划绑定读取分区表时的设备序列号。执行前再次校验序列号和连接状态，序列号变化则直接失败，见 `archive/csharp/src/VivoKsu.App/Services/PartitionExecutionService.cs:182`。
- **Rust 问题**：执行时调用 `retarget_execution_plan`，把计划中的序列号改成当前唯一在线设备序列号，见 `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/src/quick_flash.rs:85` 与 `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/quick_flash.rs:1360`。
- **影响**：用户读取 A 机分区表后，如果连接到 B 机或设备发生替换，计划会被重定向到 B 机执行。A 机镜像/分区映射可能写入 B 机，导致变砖。
- **修复原则**：删除安全链路中的静默 retarget；执行前必须校验计划序列号与当前设备一致，不一致则要求重新读取分区表并重建计划。

### 2. 等待 fastbootd 时缺少序列号绑定与截止时间

- **C# 参考行为**：自动流程支持 `expectedSerial` 与 `waitTimeout`；等待设备时必须命中同一序列号，见 `archive/csharp/src/VivoKsu.App/Services/QuickFlashService.cs:321`、`archive/csharp/src/VivoKsu.App/Services/QuickFlashService.cs:357`、`archive/csharp/src/VivoKsu.App/Services/QuickFlashService.cs:368`。
- **Rust 问题**：`resolve_fastbootd_serial_with_probe` 无限轮询，并接受任意“唯一在线”的 fastbootd 设备，见 `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/quick_flash.rs:420`。
- **影响**：
  - 自动流程可能把 A 机修补镜像刷进 B 机。
  - 设备未进入 fastbootd 时操作会永久占用 operation gate。
- **修复原则**：为自动刷写传入原设备序列号；等待必须校验同序列号；必须有明确截止时间或用户可取消的有界等待。

### 3. Fastboot / ADB 命令全部没有超时

- **C# 参考行为**：
  - 刷写类命令：10 分钟无进展超时，见 `archive/csharp/src/VivoKsu.App/Services/FastbootCliRunner.cs:17`。
  - `getvar` 探测：20 秒超时，见 `archive/csharp/src/VivoKsu.App/Services/FastbootCliRunner.cs:19`。
  - `erase` / `reboot` / `set_active`：60 秒超时，见 `archive/csharp/src/VivoKsu.App/Services/FastbootCliRunner.cs:21`。
- **Rust 问题**：所有执行点传 `timeout: None`，见 `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/quick_flash.rs:113`、`src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/quick_flash.rs:341`、`src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/quick_flash.rs:946`、`src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/quick_flash.rs:1597`。
- **影响**：fastboot/USB 卡死或半断开时，命令可能永久挂起；用户界面持续执行中，后续操作无法进入。
- **修复原则**：按命令类别设置墙钟或无进展超时。flash 采用长时间无进展保护，probe 和短命令采用严格墙钟超时。

### 4. Vivo 固件提取允许选择任意镜像，脱离 C# 受控分区白名单

- **C# 参考行为**：只允许 `boot`、`init_boot`、`vendor_boot`、`lk` 四类受控镜像进入快速刷写，见 `archive/csharp/src/VivoKsu.App/Models/FirmwarePackageInspection.cs:11` 与 `archive/csharp/src/VivoKsu.App/Services/FirmwarePackageExtractionService.cs:21`。
- **Rust 问题**：`export_zip_images_with_cancel` 允许导出任意 `.img` 条目，见 `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/src/firmware_extract.rs:686`。
- **影响**：如果结果进入快速刷写，Rust 的 `FirmwareExtractionRuntime` 会丢弃不受控结果，表现为空 `result_id`；但如果进入分区工作区批量刷写，任意镜像可被映射到同名分区并被刷写。
- **修复原则**：Vivo 固件提取阶段向 UI 只暴露受控镜像，或至少在提取前校验选择集全部在白名单内。

### 5. Fastboot 分区表读取失败可能被误报为空表成功

- **C# 参考行为**：至少一个分区大小读取成功才认为分区表有效；`getvar` 全部失败时抛错，见 `archive/csharp/src/VivoKsu.App/Services/FastbootPartitionService.cs:43`。
- **Rust 问题**：`parse_fastboot_partition_table` 对 `partitions.is_empty()` 不报错，见 `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/src/partition_workspace.rs:160`。
- **影响**：设备不可达或该 fastboot 不支持 `getvar all` 时，应用可能提示“已读取分区表”但实际是空表，掩盖连接失败。
- **修复原则**：Fastboot 分区表解析结果为空时必须失败，并给出设备可能断开或 fastboot 不支持的错误提示。

### 6. Fastboot 成功判断只看退出码，不解析输出中的失败

- **C# 参考行为**：非零退出码异常中包含完整 CLI 输出，并区分 missing partition 与连接/探测失败，见 `archive/csharp/src/VivoKsu.App/Services/FastbootCliRunner.cs:104` 与 `archive/csharp/src/VivoKsu.App/Services/FastbootCliRunner.cs:140`。
- **Rust 问题**：仅 `exit_code == 0` 即视为成功，见 `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/quick_flash.rs:1517`，未检查输出中的 `FAILED` 或 remote error。
- **影响**：部分 fastboot 实现或协议错误场景可能返回 0 但带 `FAILED`，Rust 会继续执行后续分区，造成半刷。
- **修复原则**：命令退出后检查 stderr/stdout 中的 `FAILED`、remote error；确认无法归类为良性输出时必须失败并终止后续任务。

## 中风险问题

### 7. 快速刷写错误信息缺少上下文

- **Rust 问题**：错误信息主要拼 `command.program`、退出码和 stderr，见 `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/quick_flash.rs:1517` 附近。
- **C# 参考行为**：按通道、分区和阶段包装 `PartitionOperationException`，见 `archive/csharp/src/VivoKsu.App/Services/PartitionExecutionService.cs:109`。
- **修复原则**：保留 transport、partition、stage 上下文，方便判断卡在哪个分区和阶段。

### 8. 旧设备 `is-userspace` 探测失败的兼容语义较弱

- **C# 参考行为**：旧 bootloader 对 `is-userspace` 返回 `FAIL unknown variable` 时按非 fastbootd 处理，不中断等待循环，见 `archive/csharp/src/VivoKsu.App/Services/QuickFlashService.cs:378`。
- **Rust 现状**：仅在 `read_fastboot_variable_with_probe` 成功时检查 `is-userspace`，失败则继续等待，见 `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/quick_flash.rs:462`。Fastbootd 等待场景基本可用，但错误被静默吞掉。
- **修复原则**：区分“未知变量”与真实传输失败；未知变量可按非 userspace 处理，传输失败必须记录并反馈。

## 已核对未发现回归

- Vivo tar 的 GNU long name、PAX header、8GB base-256 大小字段处理与 C# 基本一致。
- Vivo 提取使用 partial 文件，成功后统一 rename；失败时清理 partial，原子性基本正确。
- ADB Root 写入前的分区路径重解析与 C# 一致。
- ADB Root 写入前的空文件、镜像大于分区、Android sparse 镜像拦截与 C# 一致，见 `src/Nwflash.Desktop/src-tauri/crates/nwflash-domain/src/partition.rs:237`。
- Fastbootd 识别、`has-slot`、`current-slot` 解析逻辑基本一致。
- `fastboot flash` / `erase` / `set_active` / `reboot` 命令均带 `-s` 指定设备。

## 修复顺序建议

1. 执行链路先移除静默 retarget，恢复同序列号校验。
2. 自动等待 fastbootd 时绑定原序列号并加截止时间。
3. 为 Fastboot / ADB 命令分类添加超时。
4. Fastboot 命令输出解析 `FAILED` / remote error。
5. Fastboot 分区表空结果报错。
6. Vivo 固件提取恢复受控镜像白名单。
7. 改进错误上下文和 `is-userspace` 探测错误处理。

## 必须的验证

- 针对每项新增单元测试：
  - 分区表序列号与执行设备不一致时必须失败。
  - 自动流程等待到另一台 fastbootd 设备时必须失败。
  - 空分区表解析必须失败。
  - `exit_code == 0` 但输出包含 `FAILED` 时必须失败。
  - Vivo 包选择 `super.img` / `vbmeta.img` 等非白名单镜像时必须失败。
  - 命令超时能终止进程树并释放操作门。
- 针对真实设备或模拟 fastboot 输出验证：
  - A/B 双槽刷写和切槽。
  - `is-userspace: yes/no` 两种设备。
  - 旧 bootloader `unknown variable`。
  - 大 Vivo tar 包提取。

## 审批要求

- **状态**：待完成，未修复。
- **任何修复尝试**：必须先向用户说明：
  1. 要修改哪个问题与文件；
  2. 具体行为如何变化；
  3. 如何避免引入误刷风险；
  4. 计划新增/修改哪些测试；
  5. 如何回退。
- **未经用户明确批准，不得修改这些问题相关代码。**
