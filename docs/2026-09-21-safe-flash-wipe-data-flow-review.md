# VIVO 线刷「清除数据」流程审查

日期：2026-09-21
范围：`SafeFlashPage.tsx` + `nwflash-tauri` / `nwflash-application` / `nwflash-infrastructure`
性质：先只读审查（§1–§5 描述**改造前**的行为），随后按用户要求实施改造（§6 为改造后的现状）

## 1. 机制：往 misc 写一个 BCB，让 recovery 在开机时清数据

不是"格式化 userdata"，而是把一段 bootloader control block 写进 `misc` 分区，
重启后由 recovery/bootloader 读到 `--wipe_data` 指令再执行清数据。

内嵌镜像 `crates/nwflash-infrastructure/assets/wipe-data.img`（524288 字节 = 512 KiB，
`include_bytes!` 编进二进制，运行时不需要外部文件）：

```
offset 0    : "boot-recovery\0"
offset 64   : "boot-recovery\n--wipe_data\n--reason=native_wipe_data_all\n\0"
offset 112+ : 全 0
```

## 2. 流程

### 2.1 入口

- `SafeFlashPage.tsx:135` 「清除数据」勾选框，默认关，与「安全刷写」「保留ROOT」「槽位」一起
  打包成 `options { is_safe_flash, is_keep_root, wipe_data, slot_mode }`。
- DTO `SafeFlashOptionsDto`（`nwflash-tauri/src/commands/safe_flash.rs:55`）**只有这 4 个字段**：
  前端无法指定 wipe 镜像路径。执行前 `prepared_safe_flash_request()` 又把
  `options.wipe_data_image_path` 覆盖成预检会话里的值 → 镜像来源完全由后端掌控
  （测试：`prepared_safe_flash_binds_only_its_generated_wipe_image`）。

### 2.2 准备阶段（写镜像）

四条来源路径（在线固件 / 本地 zip / 本地解包目录 / payload）都会调用
`SafeFlashService::resolve_wipe_data_image_path`（`application/safe_flash.rs:1635`）：

| 条件 | 行为 |
|---|---|
| `wipe_data = false` | 原样返回 `options.wipe_data_image_path`（前端恒 `None`） |
| `wipe_data = true` 且给了路径 | 用给定路径（同样只可能来自后端会话） |
| `wipe_data = true` 且没给路径 | `staging_root/wipe-data.img`，用 `write_wipe_data_image()` 写出内嵌镜像 |

`staging_root` 的存在条件：在线源恒有；本地 zip 恒有；**本地解包目录仅在 `wipe_data = true` 时创建**
（`safe_flash.rs:1414`）。写入过程**不发任何 stage 文案**。

### 2.3 执行阶段（写设备）

队列顺序（`execute_with_partition_failure_hook`，`safe_flash.rs:456-484`）：

```
[flash 各分区 …] → [slots=对槽 时的 set_active] → [wipe: flash misc <staging>/wipe-data.img] → [reboot]
```

wipe 这一步的标记是 `is_flash = true, is_partition_flash = false, simulated_flash_bytes = None`：

- **永远是真刷**：`misc` 不在受保护分区名单里，不会被假刷写改造影响（安全刷写/保留 ROOT 都不改变它）。
- 计入 `flashed_partition_count` → 完成文案「已刷入 N 个分区」把 misc 也算了一个。
- **不计入 `partition_total`** → `刷写分区[i/n]` 的 n 不含 misc。
- **不产生任何阶段文案**：既不报 `刷写分区[i/n]`，成功/失败都没有单独日志 → 界面上看不到这一步。
- 失败：`is_partition_flash = false` → 不进「分区失败」决策弹窗，直接 `Err` 结束，
  **且不执行 reboot**（由 `partition_failure_hook_ignores_failures_of_recovery_wipe_and_control_commands` 锁定）。
- 取消：循环每步前查取消，命中则后续命令（含 reboot）都不执行。
- 收尾：成功 → `cleanup_safe_flash_staging(Success)` 连同 `wipe-data.img` 一起删；失败保留 staging 供重试。

## 3. 与 C# 参考实现（`archive/csharp/src/VivoKsu.App`）的差异

| 项 | C#（参考） | 当前 Rust |
|---|---|---|
| 确认弹窗 | `ConfirmSummary += "完成后将清除设备数据。"` | **完全没有提示**（弹窗只有分区数 + 「确认后请保持设备连接」） |
| 准备阶段 | stage「正在准备数据清除」，并特意提前做（"尽早失败，避免刷一半才发现资源缺失"） | 静默写镜像 |
| 执行阶段 | stage「正在执行数据清除」+ log「数据清除完成」 | 无任何文案 |
| misc 不可用 | 先 `PartitionExistsAsync`，不存在 → Warning「数据清除未完成,设备分区不可用。」→ **继续 reboot** | 直接 flash；失败 → **整轮中止、不 reboot** |
| 镜像内容 | `recovery\n--wipe_data\n--reason=wipe_data_from_ota`，且 offset 2048 处残留 AB 元数据（`_a` `BCAB…`） | `boot-recovery\n--wipe_data\n--reason=native_wipe_data_all`，2048 之后全 0 |
| 镜像大小 | 524288 | 524288（一致） |

> 两代镜像 `reason` 串不同（`native_wipe_data_all` vs `wipe_data_from_ota`），
> 且 Rust 版把 2048 起的旧元数据清零了。Rust 资源 mtime 2026-08-22、仓库只有一次重建提交，
> 无法从 git 判断是刻意更换还是搬运时重新生成——**建议真机验证清数据是否真的触发**
> （开机后进 recovery 是否执行 wipe，或被 `reason` 白名单拒绝）。

## 4. 真机实据（D1 run `v1:440`，`docs/2026-09-20-audit-run-v1-440.md`）

- 那次刷写的 31 个固件分区里**没有 misc**，也没有 `getvar partition-type:misc`
  → 该次 **未勾选清除数据**。
- 31 条 `getvar partition-type:<分区>` 全部 `exit=0` + `FAILED (remote: 'Invalid partition')`：
  fastbootd 下这个变量本来就无效，**连真实存在的 system 也判「不存在」**——旧的存在性校验
  实际上恒为「存在」，是空转（这正是删除它的依据）。

## 5. 问题清单（审查时的状态）

1. ~~**清数据没有任何警示/二次确认**~~ → 已改，见 §6。
2. ~~**清数据过程对用户不可见**~~ → 已改，见 §6。
3. ~~**misc 失败 = 整轮中止且不重启**~~ → 已改（不再写 misc），见 §6。
4. ~~**预检计数 off-by-one**~~ → 已改，见 §6。
5. **项目文档已过时**：`docs/2026-09-03-safe-flash-pipeline-and-logging.md` 仍描述
   `is_partition_included` 过滤、`getvar partition-type` 存在性探测、「跳过不存在分区」stage
   与「wipe_data 刷 misc」——这些都已删除，需要同步。
6. 小项：`embedded_assets` 模块与 `wipe_data_size_bytes()` 已随机制一并删除。

## 6. 已实施改动（2026-09-21，按用户要求）

把「清除数据」从「往 misc 写 BCB + 正常重启」改为「重启到 REC + 用户手动清除」：

### 6.1 行为

- 执行队列：`flash×N → [对槽 set_active] → fastboot reboot recovery`。
  **不再写 misc**，也不再追加普通 `reboot`（`reboot recovery` 之后设备已离开 fastboot，
  再派发任何 fastboot 命令都是错的）。
- 走到这一步时日志（`report_stage`，同时进操作阶段面板与服务端审计明细）报：
  `重启到REC（进REC后电脑就检测不到设备了）。请手动执行：清除数据-清除全部数据-确定-重启`
  （常量 `nwflash_application::SAFE_FLASH_WIPE_DATA_MANUAL_STEPS`）。
- `reboot recovery` 失败**只提示、不算失败**（`SafeFlashStep::tolerant_control`）：
  报「未能自动重启到REC，请手动重启到 REC 后执行：清除数据-清除全部数据-确定-重启」，
  整轮仍按完成收尾，不进分区失败决策弹窗（设备只是没自动进 REC，用户手动进同样能清数据）。
- 确认弹窗在勾选「清除数据」时新增一行：
  「清除数据：刷写完成后会重启到 REC。进 REC 后电脑就检测不到设备了，请手动执行：清除数据-清除全部数据-确定-重启。」

### 6.2 删除的机制

`wipe_data_image_path`（`SafeFlashBuildOptions` 与 `SafeFlashPreparedSource` 两个字段）、
`SafeFlashService::resolve_wipe_data_image_path`、`WIPE_DATA_PARTITION`、`WIPE_DATA_FILENAME`、
`map_embedded_asset_error`、`nwflash-infrastructure::embedded_assets` 整个模块
与 512 KiB `assets/wipe-data.img`（C# 参考副本仍在 `archive/csharp/src/VivoKsu.App/Assets/wipe-data.img`）。
`wipe_data` 布尔开关保留——它现在唯一的作用是把队列最后一步换成 `reboot recovery`。

### 6.3 顺带修掉

- 预检计数 off-by-one：`build_plan` 不再把 misc 追加成任务 → 勾清数据不再显示「32/31」。
- 「已刷入 N 个分区」不再把 misc 计入（那是写入类命令的口径，wipe 现在根本不是写入）。

### 6.4 新增/改写的测试

- `wipe_data_queues_a_recovery_reboot_as_the_last_step`：队列末尾是 `reboot recovery`、
  全程没有 misc、日志含手动步骤文案。
- `reboot_recovery_failure_is_reported_without_failing_the_workflow`：失败后 `execute` 返回 Ok，
  队列 `[flash boot, reboot recovery]` 共 2 步、只有 flash 计入成功（`executed=1`），
  兜底提示已上报，决策回调未被触发（分区失败弹窗不会弹出）。
- `partition_failure_hook_ignores_failures_of_control_commands`：普通收尾 `reboot` 失败仍按原语义中止。
- `safe_flash_build_plan_ignores_the_wipe_data_flag`：勾不勾清数据，计划完全相同。
- 删除：`safe_flash_build_plan_appends_wipe_task_last`、
  `safe_flash_build_plan_rejects_missing_wipe_data_path`、
  `prepared_safe_flash_binds_only_its_generated_wipe_image`（都不再有对应行为）。

## 7. 复审结论（2026-09-21 二次审查）

### 7.1 本轮修掉

- `execution_uses_the_sole_fastbootd_device_after_transition_and_flashes_every_partition`
  里有一段**重复的断言块**（改测试时留下的残迹：同一组 `flashed/skipped/commands[0]/
  commands[1]/fastboot_commands` 断言写了两遍）——删除重复，断言口径不变。

### 7.2 需要你确认的两点（非本轮引入，属同一批未提交改动）

- **分区存在性校验已从执行链移除**（`fastboot_partition_exists` / 「跳过不存在分区」/ `skipped_partition_count` 的该来源）。
  现在不再逐分区 `getvar partition-type:<分区>`，所有目标一律进队列。
  删它的依据是**真机证据**（见审查 §4 与 run 440）：fastbootd 下
  `getvar partition-type:*` 恒定 `exit=0` + `Invalid partition`，连真实存在的 `system` 也如此；
  而原实现的判定是「`exit_code == 0` 即存在」，只有非零退出码 + missing 文案才算不存在
  ⇒ 那条「跳过不存在分区」分支在真机上**永远走不到**，是纯空转（每个分区还多一次 fastboot 往返）。
  所以这是**实际行为无损**的简化，不是回归；`skipped_partition_count` 现在只由用户在分区失败弹窗选
  「继续刷写」产生。
  唯一的理论差异：若将来某机型 `getvar` 按预期返回非零 + missing，则「静默跳过」不会再发生，而是变成
  一次刷写失败（有弹窗可继续）。要不要把这个失败识别成「分区不存在 → 跳过并记日志」由你定。
- **预检文案「可刷写分区：{plan}/{partitions}」两个数不同量纲**：`partition_count` 是固件里的
  分区个数，`safe_partition_count` 是计划里的**刷写目标个数**（对槽/双槽会 ×2 或只算对槽）。
  对槽/双槽模式下会出现「62/31」这类数字。勾清数据导致的 32/31 已随 §6.3 修掉，但量纲问题
  与清数据无关，属既有显示口径。建议改成「可刷写分区 {n} 个（其中受保护 {m} 个）」这类单一量纲表述。
