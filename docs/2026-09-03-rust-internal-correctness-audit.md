# Nwflash Rust 版自身正确性审计报告（2026-09-03）

状态：**审计完成（只读），未修改任何代码**

> 审计基准：**纯 Rust 版自身正确性**（不与 C# 比对——按用户指示，C# 版也有 BUG 不作金标准）。静态审查全部 6 个 crate（约 63,800 行生产代码），宿主机无 cargo 工具链，结论基于逐行推理；标注"待验证"的项需真机或编译验证。
> 与前两轮报告（`2026-09-02-nwflash-vivo-line-flash-bugs.md` 行为分歧清单、`2026-09-03-nwflash-audit-report.md` 交叉审计）的问题**不重复**，本轮只收纯 Rust 自身缺陷。
> **任何修复须先向用户说明变更/影响/验证/回退并获批准。**

## 总体结论

代码质量整体高：生产路径零 `unwrap/expect`（trace 体系三个高密度文件的 496 处 unwrap 全部在测试内）、std Mutex 无跨 await 持锁、进程双管道并发排空无死锁、zip 路径穿越/TLS pin/trace 脱敏/spool 崩溃安全等核心防线经逐项核验全部有效。但本轮仍发现 **1 条会 panic 的真实缺陷、8 条功能级缺陷、若干健壮性问题**，其中三条在刷写链路上有安全后果。

---

## S1 — panic / 崩溃（1 条成立）

### R1【S1】`split_device_line` 字节切片 `line[..15]` 非 ASCII 输入直接 panic

- 位置：`nwflash-domain/src/device.rs:113`（`split_device_line`）
- 代码：`if line.len() >= 15 && line[..15].eq_ignore_ascii_case("list of devices")` —— `len() >= 15` 只防越界，不保证 15 是 UTF-8 char boundary。
- 触发：`adb devices -l` / `fastboot devices -l` 输出中任一行前 15 字节内有多字节 UTF-8 字符跨越第 15 字节（USB 序列号/产品名由**设备端描述符控制**，恶意设备或含非 ASCII 序列号的厂商设备即可命中）。
- 后果：`byte index 15 is not a char boundary` panic → 设备枚举命令崩溃；对恶意设备构成插线即 DoS 的攻击面。
- 修复原则：`line.as_bytes().get(..15).is_some_and(|s| s.eq_ignore_ascii_case(b"list of devices"))`。

## S2 — 功能错误 / 安全（8 条成立）

### R2【S2·刷写安全】`compute_targets(OtherSlot)` 槽位缺失时静默降级为裸分区名（刷当前槽）

- 位置：`nwflash-domain/src/safe_flash.rs:43-56`（`append_slot` 的 `unwrap_or_else` 回退）
- 可达路径（本轮亲自核实）：tauri 层 `secure_options`（`nwflash-tauri/src/commands/safe_flash.rs:338-347`）**固定传 `current_slot: None`** → 预检 `build_plan`（`nwflash-application/src/safe_flash.rs:544-629`）无校验直接调 `compute_targets`。用户选择 OtherSlot 模式时，预检确认页把目标算成裸分区名（fastboot 裸名 = **当前活动槽**）。执行链路（`safe_flash.rs:173-187`）虽有校验，但预检展示与执行目标不一致。
- 后果：用户要求"刷另一槽"，预检却按"刷当前槽"计算分区数与目标——另一槽镜像写进当前活动槽的展示基础；交互链路 `safe_flash.rs:248` 还会在 `other_slot` 为 None 时静默不追加 `set_active`（刷了另一槽却不切换）。
- 修复原则：删除 `append_slot` 回退分支；`compute_targets` OtherSlot 模式槽位非法时报错而非降级；`secure_options` 应在预检时读取并校验 current-slot。

### R3【S2·刷写安全】`validate_device_path` 放行 `..`，`/dev/block/` 前缀防线形同虚设

- 位置：`nwflash-windows/src/device_transport.rs:324-358`（`is_device_path_char` 允许 `.` 与 `/`）
- 触发：设备路径 `/dev/block/../sda`（或更深 `../..`）通过全部校验（前缀✓、字符集✓），随后进入 root `dd of=` / `blkdiscard` / 备份 `dd if=` 命令（`build_adb_root_copy_staged_file_to_device_command`、`build_adb_root_erase_command`、`build_adb_root_copy_from_device_command`）。分区表数据由设备端控制，恶意设备可上报构造好的 `device_path`。
- 后果：以设备端 root 权限擦除/覆写**整盘**（`/dev/block/../sda` → `/dev/sda`）或读取任意块设备；"限制到分区设备"这一安全边界完全失效。
- 修复原则：路径逐组件拒绝 `.`/`..`（或要求 `/dev/block/<name>` 且 name 仅 `[A-Za-z0-9_-]`）。

### R4【S2·任意文件写】备份输出路径用设备可控分区名直接拼接

- 位置：`nwflash-domain/src/partition.rs:162-175`（`format!("{}\\{}.img", dir, partition.name)`）
- 可达路径（本轮亲自核实）：`partitions.rs:469/506` → `partition_workspace.rs:142-157` `resolve_selected` 仅按名字匹配快照分区，无字符白名单；fastboot 表解析（`parse_fastboot_partition_table`，`partition_workspace.rs:160-208`）对 name 只查 `is_empty`。恶意设备上报 `..\..\Users\Public\evil` 形式的"分区名"即可穿越。
- 后果：备份文件写到输出目录之外任意路径（覆盖用户文件）；`\\` 硬编码还导致非 Windows 平台行为错误（`partition.rs` S3-1）。
- 修复原则：分区名 `[A-Za-z0-9_-]` 白名单后再拼路径；用 `Path::join`。

### R5【S2·驱动安装】`\\?\` verbatim 路径进入 ShellExecuteExW/pnputil，提权安装链路可能整体失效

- 位置：`nwflash-windows/src/driver.rs:782-784, 834-836, 804-811, 1004-1013`
- 机制：`canonicalize()` 在 Windows 返回 `\\?\C:\...` 前缀路径，同时进入 `ShellExecuteExW` 的 `lpFile`/`lpParameters`（runas 提权）与 pnputil 的 INF 参数。`\\?\` 前缀按 MSDN 仅适用于直接传给文件系统 API 的路径，ShellExecute/SetupAPI 无支持承诺。
- 后果：驱动安装（UAC 提权 + pnputil）在部分/全部 Windows 版本上失败（"找不到 INF"或启动失败），行为随版本漂移。**待真机验证一次 `driver_reinstall` 即可定谳**。
- 修复原则：canonicalize 后剥 `\\?\` 前缀（或直接复用构造期已校验的原始路径）。

### R6【S2】固件提取进度统计：`failed`/`skipped` 恒为 0，终态 `successful` 虚增

- 位置：`nwflash-tauri/src/commands/firmware.rs:170-188`（`update_partition_stats`）
- 机制：`FirmwarePartitionStats` 的 `failed`/`skipped` 无任何递增路径；`report_terminal` 一律 `successful = completed`。
- 后果：payload/远程提取部分失败时 UI 显示全成功（`firmware:progress` 的 failedPartitions/skippedPartitions 恒 0）。
- 修复原则：提取服务回传每分区终态，按真实计数。

### R7【S2】`device_refresh` 手动刷新路径的自动投屏 reconcile 必然失败（死锁式设计错误）

- 位置：`nwflash-tauri/src/commands/device.rs:546-576`
- 机制：`try_acquire_idle` 的 permit 存活到函数返回，`reconcile_after_device_update` → `coordinator.run_async` 的 `try_acquire_owned` 必然 `InProgress`；错误被 `let _ =` 吞掉。3 秒自动循环无此问题（lease 已释放）。
- 后果：开启自动投屏时，手动刷新设备永远无法触发投屏启动，且无任何日志提示。
- 修复原则：调用 reconcile 前 `drop(_idle)`。

### R8【S2】固件提取完成后整镜像同步复制在 async 线程执行、不受操作门保护、不可取消

- 位置：`nwflash-tauri/src/commands/firmware.rs:452-511`（`replace` → `copy_image_snapshot` 8KB 同步复制）；调用点 `firmware_extract_vivo_local:1526` 等
- 机制：`run_async` 释放 permit **之后**才执行复制（每 GB 数秒~数十秒），占用 tokio worker、无进度事件、`operation_cancel` 无效。
- 后果：大固件提取后 UI 长时间无反馈，命令 promise 挂起，阻塞其它命令的调度。
- 修复原则：复制移入 `run_async` 闭包（`spawn_blocking` + cancellation）。

### R9【S2】OTA 下载便捷入口：裸 `reqwest::Client::new()` 跟随重定向（含 https→http）且无内容哈希校验

- 位置：`nwflash-infrastructure/src/ota_download.rs:633`（`download_to_file`）；生产调用点 `nwflash-application/src/safe_flash.rs:1078`
- 机制：与 API 客户端的 `redirect(Policy::none()) + SPKI pin`（`pinned_tls.rs:437-447`）形成反差，该入口默认跟随最多 10 次重定向、允许降级 http；完整性仅靠 TLS + 长度校验（`RomResolveResponse` 的 sha256 字段未被此入口使用）。
- 后果：被劫持的下载链路可用同长度 zip 替换固件包，随后被解压刷写。
- 修复原则：`redirect(Policy::none())` 或 host 白名单 + 启用上游 sha256 校验。

## S3 — 健壮性（合并去重后 14 条）

| # | 位置 | 问题 | 修复原则 |
| --- | --- | --- | --- |
| R10 | `nwflash-infrastructure/src/vivo_firmware.rs:341` | tar `padded_size` 的 `size + pad` 可被恶意 header 溢出（release 静默 wrap → 解析错位；debug panic） | `checked_add` 失败报 `Truncated` |
| R11 | `nwflash-infrastructure/src/remote_firmware.rs`（全文件） | 唯一无 HTTP 总超时的网络模块，远端无响应可挂起数分钟 | `.timeout(30s)` |
| R12 | `nwflash-tauri/src/lib.rs:876-925` | 操作事件循环把 broadcast `Lagged` 当终止条件，事件流静默永久死亡（`report_partition_task` 绕过节流加剧） | `loop`+match 处理 `Lagged => continue` |
| R13 | `nwflash-application/src/operation_coordinator.rs:840-863` | `update()` 每次 spawn 无序任务，终态快照可能乱序（`set_completed`/`set_idle` 倒序、Idle 携旧 operation_id） | 有序通道单消费任务 |
| R14 | `nwflash-windows/src/process.rs:490-513` | 成功路径 2s reader 宽限：孙进程继承管道句柄（adb server fork）时正常命令被误报"读取超时"失败 | 子进程已退出时按成功+截断处理 |
| R15 | `nwflash-windows/src/process.rs:1229` | taskkill 硬编码 `C:\Windows\System32`，系统盘非 C:/精简系统时树杀静默失败 | 复用现成 `system_directory_path()` |
| R16 | `nwflash-windows/src/driver.rs:119-131, 1035-1060` | 驱动安装取消语义名不副实：UAC/pnputil 阶段无法取消（100ms 无限轮询无取消出口），多 INF 逐个提权 | `run_elevated` 加取消回调 |
| R17 | `nwflash-windows/src/driver.rs:137-144` | 安装成功但临时目录清理失败被报为安装失败（用户重复重装、重复 UAC） | 清理失败降级警告保留 Ok |
| R18 | `nwflash-windows/src/driver.rs:164-184` | staging guard 句柄未 drop 就 `remove_dir`（sharing violation）→ `%TEMP%\NWflash\drivers` 垃圾目录累积 | 删除前显式 drop guard |
| R19 | `nwflash-tauri/src/commands/firmware.rs:1338-1385` | `clear()` 在 permit 获取前执行：被 `InProgress` 拒绝的命令仍删掉上一次提取结果 | clear 移入授权后闭包 |
| R20 | `nwflash-domain/src/device.rs:145-211` | 设备状态用 `contains` 匹配整行余部，`-l` 的 product/model 列（设备可控）出现 "fastboot"/"unauthorized" 字样即误判通道 | 只取状态首 token |
| R21 | `nwflash-domain/src/safe_flash.rs:58-60` | `lk` 精确匹配 vs `preloader` 子串匹配不对称：`lk_a`/`lk_b` 变体不被安全跳过 | 前缀+边界统一判定 |
| R22 | `nwflash-domain/src/partition.rs:126-142` | `build_write` 对缺镜像路径的分区静默跳过（用户以为刷了 N 个实际 M 个） | 缺路径报错列出缺失项 |
| R23 | 泄漏类：`scrcpy_provisioner.rs:658` `.backup-*` 孤儿目录、`ota_download.rs:117` 崩溃残留 `.partial-*` 永不清理、`trace_facade.rs:762` registrations HashMap 无界增长、`usage_reporter.rs:128` pending 无上限且锁内同步 persist+fsync | 各自清理/上限策略 |

另记录两项**待真机验证**：R14（adb server fork 句柄行为）、R5（`\\?\` 前缀在当前目标 Windows 版本是否实际失败）；以及 R24（次要）：`exec-out su -c` 多参数拼接依赖 Magisk/KSU 系 su 语义（`device_transport.rs:50-62`），建议合并单参数。

## 本轮核验排除的怀疑（勿重复审计）

- **std Mutex 跨 await**：全仓精确扫描仅 2 处可疑，均为块作用域内已释放的误报（`operation_coordinator.rs:567`、`exit_supervisor.rs:402`）；infrastructure 全 crate 无一例。
- **trace 体系 panic 面**：三个高 unwrap 密度文件生产路径零 unwrap；`trace_spool.rs` 少量生产 unwrap 均为前置校验保证的不可达查找。
- **执行循环索引对齐**：`resolution_tasks[index]` 与 `task_commands` 同源于 `plan.tasks`，无越界。
- **管道死锁**：`spawn_observed_pipe_reader` 双路并发排空 + `try_wait` 监督循环，无先 wait 后读死锁；超时/取消路径有界 reap（2s）不挂死 coordinator。
- **zip 穿越面**：`payload_provisioner.rs`（最严：拒绝保留名/NTFS 流/尾点空格）、`scrcpy_provisioner.rs`（双防线）、`remote_firmware.rs`/`firmware_extract.rs`（`file_name()` 剥离）——全部闭合。
- **TLS/加密**：`pinned_tls` 是 WebPki 链校验**之上**叠加 SPKI pin、`https_only`、`no_proxy`、`redirect(none)`、`NoKeyLog`；`lease.rs` 先验签、常量时间 token 比较、时间窗与防重放正确；`trace_redaction` 全部切片落在 char boundary、脱敏状态机边界完备。
- **ACK/CAS 状态机**：旧 ACK 对新 attempt/revision 只记 stale 不误删，有测试覆盖。
- **7z/驱动解压**：reparse 三重防护 + 双向对账 + 编译期 SHA256 预绑定。

## 修复优先级建议

1. **第一批（小改动、高收益）**：R1（一行字节比较）、R2（删回退分支+报错）、R3（路径组件校验）、R4（白名单+join）、R7（drop 一行）、R10（checked_add）、R12（loop+match）。
2. **第二批（需真机验证定级）**：R5（`\\?\` 剥前缀，一次 driver_reinstall 验证）、R14（adb server 句柄行为）。
3. **第三批（结构性）**：R8（复制入操作门）、R9（重定向策略+哈希）、R6（分区终态回传）、R16（提权取消）。
4. 任何修复前按项目约束获得用户批准；本报告不构成修复授权。
