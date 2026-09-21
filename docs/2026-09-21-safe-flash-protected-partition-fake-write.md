# 安全刷写：受保护分区「留在队列 + 不出真命令」改造

日期：2026-09-21
范围：`src/Nwflash.Desktop/src-tauri/crates/{nwflash-domain,nwflash-application,nwflash-tauri}`
状态：已实现并被既有 + 新增测试覆盖（`cargo test -p nwflash-domain -p nwflash-application -p nwflash-tauri` 全绿）

## 1. 需求（用户原话要点）

| 分区类别 | 未勾选「安全刷写」 | 勾选「安全刷写」 |
|---|---|---|
| `lk` / `lk_…` / `lk<数字>`、名称含 `preloader`（大小写不敏感） | 不写设备 | 不写设备 |
| `system` `product` `vendor` `odm` `system_ext` `odm_dlkm` `system_dlkm` `vendor_dlkm` | 正常刷写 | 不写设备 |
| 其他分区 | 沿用现有逻辑 | 沿用现有逻辑 |

追加要求（第二轮）：「保留 ROOT」勾选时 `boot` / `init_boot` / `vendor_boot`
（含带槽位后缀的 `boot_a`/`init_boot_b`）**也走同一套假刷写**——留在队列里、
日志照常显示刷入，但不写设备。

- 只做假刷写的分区**仍要进刷写队列**，日志照常显示刷入，客户端不得看出是假刷写。
- 假刷写时长 = 镜像大小 ÷ **35 MB/s**。
- **「假戏真做」（第三轮）**：这些分区的镜像也要**真的解包**到临时目录，
  解包进度、耗时与临时占用与真机刷写完全一致；唯一被模拟的是「写进设备」那一步。
- `getvar partition-type:<分区>` 的存在性校验**直接删除**（退出码 0 也带
  `FAILED(remote: 'Invalid partition')`，判存在本身不可靠）。

## 2. 实现要点

### 2.1 判定（domain）

`crates/nwflash-domain/src/safe_flash.rs`：

- `should_simulate_safe_flash_partition(name, safe_flash)` —— 安全刷写规则。
  lk/preloader 与 `safe_flash` 无关恒为真；八个系统分区仅在 `safe_flash = true` 时为真。
- `should_simulate_keep_root_partition(name)` —— `boot` / `init_boot` / `vendor_boot`。
- **`should_simulate_partition_flash(name, safe_flash, keep_root)`** —— 应用层唯一入口，
  两者取并集。三条都按**去掉 `_a`/`_b` 后缀的基名**精确匹配，
  `my_system`、`systemui`、`lksec`、`my_boot`、`boot_c` 不得误伤。

### 2.2 队列构造（application）

`crates/nwflash-application/src/safe_flash.rs`：

- `SafeFlashPartitionSource` 新增 `simulated_flash_bytes: Option<u64>`：
  `Some(n)` = 该分区只做假刷写，`n` 是计时用的镜像大小（取**落盘镜像的真实大小**）。
- 过滤整体取消：`is_partition_included` / `SafeFlashService::is_boot_partition`
  已删除，**任何来源都不会再把分区从清单里剔掉**，是否假刷写只在执行阶段判定。
- 四条来源解析路径**照常解包全部镜像**（与关闭安全刷写时完全一致）：
  - zip：不再有「跳过解包」的分支，逐条目写入 staging 后 `fs::metadata().len()`
    作为模拟大小；`zip_extraction_output_size` 也把全部条目计入解包空间校验。
  - payload：`PayloadEntryPlan` 分流被删除，`selected = inspection.entries`
    全量交给 payload_dumper，模拟大小取 `FlashImageInfo::size_bytes`。
  - 目录源 / 单镜像源：文件本就在本地，用 `fs::metadata().len()` 计时。
- 执行队列由 `(ProcessCommand, bool, bool)` 三元组改为 `SafeFlashStep`
  （`is_flash` / `is_partition_flash` / `simulated_flash_bytes`）。
- `fastboot_partition_exists` 与 `is_missing_partition_error` 整体删除，
  不再有任何 `partition-type` 探测；分区不存在时由 `flash` 命令自身失败，
  仍走既有的「重试 / 继续 / 中止」弹窗流程。

### 2.3 假刷写执行

```rust
mod simulated_flash {
    pub const SPEED_BYTES_PER_SECOND: u64 = 35 * 1024 * 1024;
    pub const SLICE: Duration = Duration::from_millis(50);
    pub fn duration(bytes: u64) -> Duration { /* bytes × 1000 / 35MB，ms */ }
}
```

- `run_simulated_flash(bytes, is_canceled)`：**不派发任何命令**，只按
  `duration(bytes)` 分片 `sleep`，每片之间检查取消 →「停止操作」立刻生效
  （350 MB 的模拟等待 10 s，实测取消后 ~0.2 s 收尾）。
- 返回与真实成功刷写同样的 `ProcessOutput { exit_code: 0 }`，
  因此日志文案、`flashed_partition_count`、`executed_command_count` 与真机一致。
- 日志仍是 `刷写分区[i/n] ...` / `... OK`，**没有任何**「假 / 模拟 / 跳过」字样。

### 2.4 顺带修掉的既有缺陷

- **重试导致分区序号多加一次**：`Retry` 分支原来只回退 `index`、没回退
  `partition_index`，会出现 `刷写分区[3/2]`；现在两者一起回退。
- 移除存在性校验后，预检「可刷写分区 X/Y」与实际派发数量口径一致
  （受保护分区两处都计入）。

## 3. 测试

新增：

- domain：`protected_partitions_depend_on_the_safe_flash_flag`
  （两种模式 × 八个系统分区 × `_a`/`_b` 变体 + 近似名反向用例）、
  `keep_root_boot_partitions_are_simulated_only_when_selected`
  （保留 ROOT 开关 + 槽位后缀 + 两个开关的并集）。
- application（集成）：
  - `protected_partitions_are_reported_as_flashed_but_never_written_to_the_device`
    —— 断言派发的 flash 目标只有 `userdata`，四条分区都报 `OK`，
    且日志不含暗示词、全链路无 `partition-type`。
  - `without_safe_flash_only_lk_and_preloader_are_kept_off_the_device`
    —— 模式差异：`system` 真实刷入。
  - `keep_root_partitions_are_reported_as_flashed_but_never_written`
    —— `boot` / `boot_a` 报 OK 但不派发，`userdata` 真刷。
  - `without_keep_root_boot_partitions_are_written_normally` —— 对照组，防止判定过宽。
  - `simulated_flash_wait_stops_immediately_when_canceled`。
- application（预检 / 解包）：
  - `protected_partitions_are_extracted_like_a_real_flash` —— zip 通道：勾选安全刷写后
    `system.img` 必须真的落盘、大小与 `simulated_flash_bytes` 一致，`userdata` 照旧。
  - `payload_extracts_protected_partitions_and_marks_only_them_as_simulated`
    —— payload 通道：`system` 落盘且标记模拟（大小 8），`boot` 落盘且不模拟。
- application（单元）：`simulated_flash_duration_follows_the_35mbps_rule`。

改动：`build_plan` 过滤语义测试（现在任何来源都不再剔分区）、
`execution_uses_the_sole_fastbootd_device…`（不再有「跳过不存在分区」）、
`execution_cancellation_before_the_first_flash…`（少一条 getvar），
以及全部测试桩里的 `partition-type` 输出。

## 4. 未改动 / 说明

- 「保留 ROOT」现在也走假刷写：boot 类分区会出现在刷写日志里并计入
  `flashed_partition_count`，但不会被写入设备（保护效果不变，观感与普通刷写一致）。
  这一改动同时消除了「`boot_a.img` 不被保留 ROOT 过滤」的老缺口。
- **代价**：勾选安全刷写时也要把 system/product 等大分区解包到临时目录，
  磁盘与耗时回到与「关闭安全刷写」一致的水平（vivo 固件通常多出数 GB 临时占用）；
  解包前有磁盘空间校验，不足会提前报错而不是刷到一半失败。
- 假刷写不派发命令 = 逐命令留痕（`RecordingProcessExecutor`）里也不会出现
  这些分区，服务端审计明细会少掉原本不该有的记录。

## 5. 复现命令

```bash
cd src/Nwflash.Desktop/src-tauri
export PATH="/c/Program Files (x86)/Microsoft Visual Studio/2022/BuildTools/VC/Tools/MSVC/14.44.35207/bin/HostX64/x64:/c/Windows/System32:/c/Windows:/usr/bin:/bin:$HOME/.cargo/bin"
export LIB='C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Tools\MSVC\14.44.35207\lib\x64;C:\Program Files (x86)\Windows Kits\10\Lib\10.0.26100.0\ucrt\x64;C:\Program Files (x86)\Windows Kits\10\Lib\10.0.26100.0\um\x64'
cargo test -p nwflash-domain -p nwflash-application -p nwflash-tauri
```

> `System32` 必须留在 PATH 里：payload_dumper 夹具用的是 `findstr` 等 cmd 内置命令，
> 缺了会得到「payload 读取元数据失败」这种和代码无关的假失败。
