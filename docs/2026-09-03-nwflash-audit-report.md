# Nwflash Vivo 线刷链路审计报告（2026-09-03）

状态：**审计完成（只读），未修改任何代码**

> 审计方法：对照冻结 C# 参照实现（`archive/csharp/src/VivoKsu.App/`）逐点核对 Rust/Tauri 版（`src/Nwflash.Desktop/src-tauri/`）行为，覆盖四条战线：核心线刷链路（本报告第一、二、三节）+ 三个外围域（设备会话/文件管理、Root 链/SafeFlash 协调、scrcpy/登录/OTA/心跳/用量/驱动/退出监督，由并行子审计完成并经交叉验证）。
> 机型绑定、哈希校验、UI/文案差异按用户要求不算 BUG。
> 前置结论（8 个已知 BUG 复核）见《[2026-09-02 线刷高危 BUG 待完成清单](2026-09-02-nwflash-vivo-line-flash-bugs.md)》。
> **任何后续代码修复必须先向用户说明变更/影响/验证/回退并获批准。**

## 分级汇总

| 级别 | 已知清单（2026-09-02） | 本轮新增 |
| --- | --- | --- |
| P0 变砖/跨设备 | 2 条维持 | **5 条** |
| P1 功能错误/安全 | 4 条维持（其中 1 条定性修正，见 §1.3） | **8 条** |
| P2 健壮性 | 2 条维持 | **7 条** |
| P3 低危 | — | 5 条 |
| 证伪 | — | 3 条疑点证伪 |

**本轮最重要结论**：跨设备序列号不绑定不是孤立 BUG，而是**系统性模式**——写入/擦除（已知）、备份（新发现）、双槽预检→执行（新发现）、ROOT 自动流程（新发现）、切槽/重启（新发现）、文件管理删除（新发现）、safe_flash 执行链（新发现）共 **7 条执行路径**都存在“用当前在线设备替代计划设备”的行为；C# 侧全部有 `VerifySession`/`expectedSerial` 同类保护。建议作为一号修复主题整体闭环。

---

## 一、已知 8 BUG 复核结论

全部维持 2026-09-02 清单定性（无翻案、无遗漏），关键三条：

### 1.1 `retarget_execution_plan` 执行前静默改序列号（P0，维持并扩大覆盖面）

- C# 基准：`PartitionExecutionService.cs:182-199` `VerifySession` 逐任务校验 `session.Serial == plan.Serial`，不一致即抛“连接设备已变化”；`:60-61` 执行循环内每任务前重复校验。
- Rust：`nwflash-application/src/quick_flash.rs:85-99` 静默改写；`nwflash-tauri/src/commands/quick_flash.rs:1303-1361` `resolve_execution_plan_with_discovery` 两通道均现查“当前设备”后在 `:1360` 改写。覆盖 `partitions_execute_erase/write`（`partitions.rs:477-496`）与全部快速刷写预设。

### 1.2 `resolve_fastbootd_serial_with_probe` 无序列号绑定、无截止时间（P0，维持）

- C# 基准：`QuickFlashService.cs:321-355` `waitTimeout` 截止 + `expectedSerial` 逐次比对（`:370-373`），超时明确报错并释放 gate。
- Rust：`nwflash-tauri/src/commands/quick_flash.rs:425-481` `loop { sleep(1s) }` 直到出现任一唯一 fastboot 设备，`select_waiting_fastboot_device`（`:308-318`）多台拒绝但**不校验是否为计划设备**。

### 1.3 fastboot 失败输出解析——**定性修正**（原 P0“不解析 FAILED”降为 P2“错误信息丢弃”）

交叉验证发现原清单第 3 条定性有误：**C# 基准同样只按退出码判定成败**（`FastbootCliRunner.cs:50-52, 63-65, 121-124, 134-137`；`PlatformToolsNativeApi.cs:161-171`；全 C# 工程无 "FAILED"/"ERROR" 行解析），输出仅作为错误文本拼接进异常消息。真实分歧是：
- Rust `SafeFlashExecutionService::run_required`（`nwflash-application/src/safe_flash.rs:334-361`）失败时错误消息**丢弃** fastboot 可读输出（C# 异常消息含 CLI 输出，如 `FastbootCliException` 消息格式 `... 失败:{NewLine}{output}`）；
- `is_missing_partition_error`（`safe_flash.rs:519-525`）关键字集比 C# `LooksLikeMissingPartition`（`FastbootCliRunner.cs:140-145`）少 `"not found"`，个别设备文案会漏判“分区不存在”。

修复原则：错误消息并入输出文本（脱敏后）；补齐关键字集。定级 P2（可诊断性/兼容性），非行为正确性回归。

其余维持原判，不再展开：`run_process_command` 超时恒 `None`（`quick_flash.rs:106-119` → `nwflash-windows/src/process.rs:704-719`）；zip 提取无白名单（`nwflash-infrastructure/src/firmware_package.rs`）；空分区表返回 Ok（缺 `FastbootPartitionService.cs` 的 `anyRead` 保护）。

---

## 二、本轮新增发现 — P0（变砖/跨设备/挂死级）

### N1【P0】双槽预检确认与执行之间可静默换设备（预检结论被施加到另一台设备）

- C# 基准：`QuickFlashService.cs:84-104` 预检（has-slot/current-slot）与刷写、切槽（`:138`）、重启（`:152`）全程同一 `device.Serial`；ROOT 流程 `:357-373` 用 `expectedSerial` 逐次比对。
- Rust：`quick_flash_prepare_dual_slot_preset_image`（`nwflash-tauri/src/commands/quick_flash.rs:1056-1087`）预检读 A 机的 `has-slot`/`current-slot` 并固化 tasks（`boot_a/boot_b`）与 `switch_to_slot`；执行端 `quick_flash_execute_with_plan_provider`（`:1397-1666`）经 `resolve_execution_plan` 重新发现设备并经已知 BUG 1 改写序列号——B 机的双槽结论被 A 机的预检结果直接套用。
- 影响：向不支持双槽的设备写 `*_a/_b`、或把 B 切到错误槽位。确认 DTO（`quick_flash.rs:70-74`）也不含序列号，前端确认页无法核对目标设备（P3）。
- 修复原则：双槽计划携带预检序列号，执行时强制比对，不一致要求重新预检。

### N2【P0】刷写完成后切槽/重启命令发向“重新发现的当前设备”（疑点 2 证实）

- C# 基准：`QuickFlashService.cs:126-152` `SetActiveAsync(device.Serial, ...)`/`RebootAsync(device.Serial)` 全程同一序列号。
- Rust：`build_post_flash_slot_switch_for_current_device_with_fastboot_probe`（`quick_flash.rs:569-585`）与 `build_post_flash_reboot_for_current_device_with_fastboot_probe`（`:601-619`）内部再次 `resolve_fastbootd_serial_with_probe(..., wait=false)` 现查设备，无视 `plan.serial`；执行侧 `quick_flash.rs:1581-1625`。
- 影响：单台换设备时 `set_active`/`reboot` 发到新设备——把 A 的切槽意图施加到 B（B 可能被切到无镜像的槽位无法启动）。
- 修复原则：post-flash 命令绑定 `plan.serial`；计划设备消失即报错，不重新发现。

### N3【P0】ROOT 自动流程刷写阶段不绑定预检序列号（FastbootWaitTimeout + expectedSerial 双缺失）

- C# 基准：`RootViewModel.cs:584-586, 612-614` `FlashRootImagesAsync(..., waitTimeout: 180s, expectedSerial: session.Serial)`；`QuickFlashService.cs:346-373` 注释明确“绝不能刷，等待直到同一台现身”。
- Rust：`commands/root.rs:2150-2196` FlashFastbootd 阶段 → `SafeFlashExecutionService::execute`（`nwflash-application/src/safe_flash.rs:139-322`）→ `wait_for_fastbootd`（`:363-394`）：只保证“唯一 fastboot 设备且 is-userspace=yes”，**无序列号绑定**；等待窗口 60s（`:118,125`，120×500ms）也短于 C# 的 180s。`SafeFlashExecutionRequest::serial` 的注释（`:94-98`）明示“fastboot commands target the sole device discovered after the transition”。
- 影响：A 机重启进 fastbootd 失败/超时期间接入 B 机 → B 被刷入 A 的修补 boot 镜像。这是全自动 ROOT 高危链路上的跨设备变砖路径。
- 修复原则：reboot fastboot 前捕获序列号；`wait_for_fastbootd` 等待同序列号现身，不一致继续等待直至截止；窗口对齐 180s。

### N4【P0】ADB Root 分区备份用“当前设备”执行，备份内容张冠李戴

- C# 基准：备份同样走 `VerifySession`（`PartitionExecutionService.cs:60-61, 182-199`）。
- Rust：`partitions_execute_backup`（`nwflash-tauri/src/commands/partitions.rs:499-641`）**不经** `resolve_execution_plan`，循环内 `:547-551` 直接取 `device_runtime.active_adb_serial()` 执行 `dd`；仅做分区路径解析校验（`:526-545`，同型号设备路径相同，防不了换设备）。
- 影响：换设备后“备份 A 的 boot”实际导出 B 的内容；用户后续把该“备份”恢复到 A = 把 B 的镜像刷入 A，构成变砖链。
- 修复原则：备份执行前校验 `plan.serial` 与当前设备一致。

### N5【P0】应用退出不清理 scrcpy 镜像进程 + payload 提取外部进程无进展超时（两域各一条）

**N5a 退出不杀 scrcpy（进程泄漏）**
- C# 基准：`AppComposition.cs:262` OnExit 5s 预算内 `mirrorService.StopAsync()`（`MirrorService.cs:68-84` 杀进程树+Dispose）。
- Rust：`AppStateExitCleanup::revoke_capabilities`（`nwflash-tauri/src/lib.rs:108-153`）只清 session/临时目录/token，**不含 mirror_runtime**；前端 `App.tsx:461-504` closeWindow 只调 `session_stop`/`auth_logout`。scrcpy 是独立 SDL 窗口子进程，主进程退出后残留。
- 修复原则：退出清理序列显式调用 `mirror_runtime.stop()`。

**N5b 云提取 payload_dumper 无进展 120s 判死缺失（挂死）**
- C# 基准：`PayloadDumperRunner.cs:17, 162-190` 120s 无进展（写字节推进则不杀）`Kill(entireProcessTree:true)`。
- Rust：`nwflash-application/src/firmware_extract.rs:442-453` `run_command_with_cancel(..., None, ...)` 超时显式 `None`；轮询回调只上报进度+查取消，不判死。服务器半开连接/磁盘满时操作永远“提取中”。
- 修复原则：轮询回调记录最近字节推进时间，超 120s 无进展终止进程树。

---

## 三、本轮新增发现 — P1（功能错误/安全）

### N6【P1】自动设备发现轮询无超时，单次挂起即永久停摆插拔检测

- C# 基准：`PlatformToolsNativeApi.cs:9-10, 43-47` 所有探测命令 15s 硬超时并杀进程树；心跳单 tick 有界、失败不终止循环（`DeviceMonitorService.cs:119-148`）。
- Rust：发现链 `commands/device.rs:317-346` → `platform_tools.rs:22-26` → `process.rs:740-745` 固定 `timeout: None, should_cancel: || false`；3s 心跳循环（`lib.rs:991-1010`）同步串行，一次 `adb devices` 挂起即永久停摆并占用 blocking 线程。
- 修复原则：探测命令 15s 级墙钟超时+杀树；心跳单 tick 有界。

### N7【P1】文件管理器换设备后不重置远程状态，可对新设备同路径误删

- C# 基准：`FileManagerViewModel.cs:165-192` 序列号变化即 `ResetRemoteState()`（清列表、路径回 /sdcard、撤销删除确认）。
- Rust：`FileManagerPage.tsx:31-35` 本地 state 无序列号变化监听；`files_delete`（`commands/files.rs:172-176`）用当前设备执行，`remote_path` 取自旧设备列表。
- 影响：A 浏览→换 B→对列表条目确认删除 = 删 B 的同路径文件。
- 修复原则：前端在序列号/连接状态变化时重置远程状态（等价 C# ResetRemoteState）。

### N8【P1】safe_flash 线刷执行链整链不绑定计划序列号（与 N3 同源、独立实现）

`SafeFlashExecutionService::execute` 在 `transition_to_fastbootd` 后 `wait_for_fastbootd` 接受任意唯一 fastbootd 设备，后续全部 flash/getvar/set_active/reboot 用新序列号。C# 全程 `expectedSerial`。修复原则：请求携带计划序列号，等待校验一致。（与 N3 修复合并处理。）

### N9【P1】flash 无字节级进度且无空闲超时（已知 BUG 4 的具体化，疑点 4 证实）

- C# 基准：`FastbootCliRunner.cs:249-274` `GetProcessIoCounters` 采样——`FlashAsync` 连续进度（`:37-49` bytesWritten/imageSize）+ 600s 空闲超时（有 IO 不重置不杀，慢速 USB 刷大分区不中断）。
- Rust：`nwflash-windows/src/process.rs` 全部执行器无 IO 采样、无空闲超时；`run_process_command`（`quick_flash.rs:106-119`）超时 `None`；进度只有命令间 `index/task_total` 粒度（`:1512-1516`）。
- 影响：大分区刷写 UI 无进度；fastboot 挂死时永久占用操作门。
- 修复原则：flash 类命令引入 IO 字节采样进度+空闲超时（对齐 C# 600s）；探测/短命令分级固定超时（getvar 20s / erase、reboot、set_active 60s）。

### N10【P1】`normalize_current_slot` 不校验变量名前缀（疑点 3 证实）

- C# 基准：取值链 `GetVarAsync` → `ExtractVariableValue` 按 `current-slot:` 前缀匹配行（`FastbootCliRunner.cs:148-164`）→ `NormalizeCurrentSlot` 只面对裸值（`QuickFlashService.cs:411-416`），非法即抛错。
- Rust：`quick_flash.rs:999-1010` 对拼接的 stdout+stderr 用 `rsplit_once(':')` 盲取**第一个含冒号行**的冒号后内容，不校验 `current-slot:` 前缀、不剥 `(bootloader)`；`nwflash-domain/src/quick_flash.rs:127-150` `normalize_to_slot` 同样写法。同文件 `is_true_fastboot_variable`（`:979-997`）与 safe_flash 的 `parse_fastboot_var_output`（`safe_flash.rs:484-494`）都有前缀校验，唯独此处缺失。
- 影响：杂散含冒号行可致**切槽方向算反**（切回旧槽）或误报。
- 修复原则：按 `current-slot:` 前缀匹配行后取值再规范化。

### N11【P1】设备发现 ADB 优先短路，隐藏并存 Fastboot 设备（多设备冻结保护缺失）

- C# 基准：`PlatformToolsNativeApi.cs:86-91` 合并 adb+fastboot 列表；合计>1 台 → `MultipleDevices` 冻结（`FastbootRsDeviceParser.cs:20-23`）。
- Rust：`nwflash-application/src/device_session.rs:27-35` 有 ADB 设备即短路返回，不查 fastboot 列表；同时插 ADB 手机+fastboot 设备时只报 ADB 已连接。
- 修复原则：合并两侧输出，合计超一台按 C# 语义报多设备。

### N12【P1】scrcpy crash-loop 无熔断、异常退出无退避

- C# 基准：`MirrorService.cs:14, 198-204` 连续 3 次异常退出即停止自动恢复；`:209-217` 退出后 1s 退避再 reconcile（事件驱动）。
- Rust：`commands/mirror.rs:208-224` supervision 循环发现退出直接 break；重启依赖 `lib.rs:991-1010` 3s 轮询无条件 `reconcile_after_device_update`，全仓无连续失败计数 → scrcpy 损坏时 ~3s 一次无限重启风暴。
- 修复原则：连续失败 ≥3 次停止自动恢复；异常退出后 1s 退避。

---

## 四、本轮新增发现 — P2（健壮性/兼容性）

| # | 标题 | C# 基准 → Rust 实际（file:line） | 修复原则 |
| --- | --- | --- | --- |
| N13 | 文件管理/设备短命令无超时，挂起时 UI 永久 pending 且无页内取消入口 | `PlatformToolsNativeApi.cs:93-135` 15s 级超时 → `commands/files.rs:89-97,140-146`、`device.rs:618-624`、`partitions.rs:348,375,533` 全部 `None`；`FileManagerPage.tsx` 无取消按钮 | 短命令恢复命令级超时；传输类保留取消 |
| N14 | 长操作期间自动刷新每 3s 写一条“已跳过”日志，环形日志被噪声挤出 | `DeviceMonitorService.cs:159-167` busy 静默跳过 → `device.rs:322-333` 每 3s 记 Warning（30 分钟刷写≈600 条） | busy 跳过静默化或限流去重 |
| N15 | Root 资源链运行期不配置下载器，APK 缺失/损坏无法自愈 | `AppComposition.cs:50-57` + `VivoRootResourceService.cs:107-141` GitHub 按需下载 → `commands/root.rs:1407,1488,1587`、`resources.rs:61,161` 全部 `VivoRootResourceService::new(app_root, None)`；`root_resources.rs:156-161` 直接报“缺失且未配置下载器” | 装配 `RemoteAssetDownloader`（已实现未接线） |
| N16 | scrcpy `stop()` 失败时句柄已被 `take()`，孤儿进程+双实例风险 | `SystemProcessRunner.cs:52-58` 杜绝 stale handle → `commands/mirror.rs:135,310-320` taskkill/kill 均被 `let _ =` 吞掉后 child 已移除 | 终止失败保留 child 或记录孤儿 PID，下次 start 前强杀 |
| N17 | safe_flash CurrentSlot 模式下读不到槽位时静默回退“无后缀分区名” | `QuickFlashService.cs:411-415` 非法槽位一律抛错 → `safe_flash.rs:173-187` 仅 OtherSlot 校验；`compute_targets`（`nwflash-domain/src/safe_flash.rs:36-51`）CurrentSlot 分支返回裸名 | CurrentSlot 读不到有效槽位与 OtherSlot 一致报错 |
| N18 | OTA 整包下载无分片级重试 | `OtaDownloadService.cs` `MaxTryAgainOnFailure=3` → `ota_download.rs:296-313` 任一分段失败即整体失败 | 每分段有限次重试 |
| N19 | has-slot 值解析比 C# 严格（失败关闭方向，兼容性） | `QuickFlashService.cs:407-409` `Contains("yes"/"true")` → `quick_flash.rs:979-997` 精确匹配 `yes\|true\|1\|on`，`YES (both slots)` 之类带尾缀值被拒 | 设备兼容性测试后决定对齐或保留收紧 |

**产品级行为差异（按项目文档为有意设计，列出供确认，不列 BUG）**：
- 心跳连续 3 次瞬态失败即整应用退出（`session_lifecycle.rs:150, 519-535`；C# 无限静默重试）——15s 断网即退出，属新增强制在线策略。
- 投屏期间占用 coordinator 许可、无法并行刷机（`PROJECT_PROGRESS.md:230` 明示有意）。
- 驱动安装逐 INF 循环 pnputil（`driver.rs:123-131`）vs C# 单条通配符命令（`VivoDriverInstaller.cs:94-95`）：部分失败无回滚、多次 UAC；错误码语义被折叠（C# 把 259/301 部分成功返回 UI）。
- goodbye 请求体缺 `client_version` 等字段（`api_client.rs:639-648`），需与 worker 契约核对。
- scrcpy 无 GitHub 按需下载回退，内置资源损坏即不可用（提示重装）。

## 五、本轮新增发现 — P3（低危）

| # | 标题 | 要点（file:line） |
| --- | --- | --- |
| N20 | Fastboot 模式设备详情不补 getvar（current-slot/unlocked/product） | C# `DeviceInfoService.cs:40-56` 容错补充；Rust `device.rs:456-492` 仅 ADB 补充，fastboot 分支返回裸快照，解锁状态不可见 |
| N21 | 设备监控循环无停机 token | C# `DeviceMonitorService.cs:60-97` StopAsync+排空；Rust `lib.rs:986-1011` 无 stop token |
| N22 | 双槽预检确认 DTO 不含序列号，前端无法核对目标设备 | `quick_flash.rs:70-74`（加剧 N1） |
| N23 | `mirror_stop` 后操作记为“成功”而非“已取消” | `commands/mirror.rs:216, 283-291`；C# StopAsync 无此歧义 |
| N24 | `ExtractVerifiedLibKsud` 失败路径残留 `.pending` 文件 | `root_resources.rs:295-318`；C# `VivoRootResourceService.cs:273-277` catch 兜底删除 |

## 六、证伪清单（疑点排除，防止后续重复审计）

| 疑点 | 结论 | 证据 |
| --- | --- | --- |
| 双槽 lease 释放竞态（原疑点 1） | **证伪** | `SessionCapabilityScope`（`session_capabilities.rs:192-213`）epoch 校验+commit 锁内闭包；`capture_lease` 是 epoch 快照非排他锁，无需释放；失效后延迟发布被拒有测试（`:512-537`） |
| AdbRoot 分区表解析未剥离 `ls -l` 风格（原疑点 5） | **证伪** | 发现命令两边逐字一致（`partitions.rs:24` = `AdbRootPartitionTransport.cs:9`），`|` 分 4 段协议；`parse_adb_root_partition_table`（`partition_workspace.rs:210-256`）= C# `ParsePartitions`（`:109-134`） |
| “C# 解析 FAILED 输出而 Rust 只看 exit_code”构成行为回归 | **证伪（定性修正）** | C# 同样只看退出码，全 C# 无 FAILED 行解析；真实分歧仅错误消息丢弃（见 §1.3） |

## 七、已验证一致清单（交叉验证通过，勿重复审计）

**核心链路**：`is_high_risk_partition` 高风险判定逐字一致（13 前缀+后缀规则，`nwflash-domain/src/partition.rs:293-307` = `PartitionRiskPolicy.cs`）；AdbRoot 分区表发现命令与解析一致；AdbRoot 执行前路径复核一致（`quick_flash.rs:1462-1494` = `AdbRootPartitionTransport.cs:104-114`）；预设刷写硬编码 Fastbootd 与 C# 相同（非回归）；safe_flash staging 并发唯一/取消传播/失败清理/磁盘预检齐备；zip/payload 提取白名单比 C# 更严；wipe_data（misc）检查分区存在才刷。

**设备/文件管理域**（25 项，摘要）：设备状态解析语义逐条一致（多设备/offline/unauthorized/`* daemon` 过滤）；心跳 3s、断连 2 次去抖、故障 3 次去抖、身份变化才广播、busy 跳过不覆盖权威快照、补偿刷新；文件列表/删除/APK 安装/push 拼接命令模板与解析一致；Windows 保留名/非法字符校验等价或更强；`rm -rf` 拒绝根目录与穿越（更强）；shell 引号转义安全；备份大小校验+partial 原子替换；`-s <serial>` 显式绑定齐全。

**Root 链/SafeFlash 协调域**（10 项，摘要）：ADB Root 暂存三段式（push→su dd→rm）与拒绝 stdin 管道（强化）；备份 `--no-pty dd` 防 PTY 污染；KMI 映射逐分支一致；Root 资源 SHA-256+manifest 校验一致；下载器多镜像/无进展超时/事务化提交（更强）；操作互斥 `InProgress` 立即拒绝+finally 释放；双槽目标展开顺序一致；分区存在性探测语义一致；多设备拒绝。

**scrcpy/云链路域**（12 项，摘要）：scrcpy 启动参数与环境逐项一致；心跳间隔/超时/终止语义/回调死锁防护一致；退出 closeout 750ms 预算优于 C#；用量批上传参数/失败保留/退出 flush 一致+磁盘 spool（更强）；登录/OTA 链接解析逐条一致；OTA 下载探测长度/磁盘预检/8 连接分片/Range 退化一致；云提取 206 严格校验/原子发布一致；驱动 7z 穿越防护/VID 列表/UAC 1223→取消一致；退出监督状态机自洽。

## 八、C# 无对应实现的新增功能（不算 BUG，审计时确认自洽）

会话能力 epoch 体系（`session_capabilities.rs`）；Plan C 结构化 trace spool/uploader/HTTP/VMP/租约签名体系；ExitSupervisor 退出状态机（exit-pending→terminating、panic fail-closed）；双槽/固件产物一次性预检凭据（epoch+take 防重放）；操作协调器同步派发屏障；Root OTA URL 内存化/staging 所有权移交；下载事务化回滚；操作日志脱敏；用量日志磁盘持久化+账号隔离。

## 九、修复优先级建议

1. **主题一（最高优先）：执行链全程序列号绑定**——N1/N2/N3/N4/N8 + 已知 BUG 1/2 一次性闭环：`PartitionExecutionPlan.serial` 从预检锁定贯穿到切槽/重启/备份；所有“重新发现当前设备”的调用点（`resolve_execution_plan_with_discovery`、`build_post_flash_*`、`partitions_execute_backup`、`wait_for_fastbootd`）统一改为“校验一致否则报错”。
2. **主题二：超时与进度体系**——N5b/N6/N9/N13 + 已知 BUG 4：在 `nwflash-windows/src/process.rs` 统一引入命令分级超时（探测 15-20s / 短命令 60s / flash 空闲 600s / payload 空闲 120s）+ IO 采样进度。
3. **主题三：外围健壮性**——N5a/N12/N16（scrcpy 生命周期）、N7（文件管理重置）、N10（槽位解析）、N15（下载器接线）。
4. 其余 P2/P3 按模块各自跟进。
5. **任何修复前按项目约束：先出变更说明（影响范围/验证方式/回退方案）并获用户批准；本报告不构成修复授权。**
