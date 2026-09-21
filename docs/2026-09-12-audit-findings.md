# 2026-09-12 审计发现汇总（待批量审批）

本文件汇集本轮多 agent 审计的全部待审批项。按项目操作规则：审计→报告→**用户批准后**修复；被剔除项不补修。已完成修复见文末「已落地」。

已收报告：①操作协调器语义 ②quick_flash 两段式 ③工作区盘点 ④nwflash-windows 进程/设备适配 ⑤逻辑混乱专项扫描（40 条）⑥ROOT+SafeFlash ⑦is_busy 修复对抗审查 ⑧app 壳层/退出链路 ⑨网络层/infrastructure+消费方 ⑩下载资源/固件提取/文件管理/驱动/操作日志 ⑪domain 层+跨 crate 契约（P1×3、P2×13、逻辑混乱×5；依赖方向/租约校验/trace 契约/脱敏管线等 17 项核实无问题；**确认 is_busy 修复接线一致**）⑫mirror 投屏 + drivers（**P0×2**、P1×4、P2×7、逻辑混乱×3、测试缺口×7；驱动链完整性工程远超 C# 原型等 14 项核实无问题）⑬auth/session/online/software/version/device_identity/release_probe（P1×2、P2×7、逻辑混乱×2；凭证驻留/登录租约/能力 epoch/在线隐私等 13 项核实无问题；**P1-1 正是协调员要求评估的「未发现持租约路径」——root_ota_check 是唯一新增无界路径**；P1-2 死 main.rs 解释 crash 上传为何空转）。**全部 13 份报告已收齐。**

三源确认：remote_firmware 无超时（⑨⑩⑪）；pinset 过期自砖（⑨ P1-3 + ⑪ P1-1 深化：会话中过期→心跳立即完整性退出，重启再 panic；恢复 API 全仓零接线）。

## A 组审批速览（按修复主题分组；锚点经 2026-09-12 抽查 9/9 准确；✅=09-13 已自主修复落地，见 D-11~D-22）

| 组 | 项 | 主题 | 修复代价 |
|---|---|---|---|
| 变砖/伤设备级 | A19 A20 | ROOT 全自动跨阶段换设备、线刷预检→执行换设备（quick_flash 判死 vs 线刷照刷） | 中（各加一处 serial 绑定校验；关联澄清 C-25/C-31 先拍板） |
| P0 锁死家族（⑫） | A57 A58 A61✅ A59 A60✅ A62 | 投屏持许可横跨会话→登出/退出/刷新全锁死、断流恢复烧额度关自动、failover 下载不可取消、投屏抑制刷新、重启计数复位、装后复检 | 中（A57 结构改法；A58 重启改 reconcile；A61/A60 已修；A59 随 A57） |
| 挂死/活锁 | A2✅ A7✅ A13✅ A23 A32 A33✅ A34✅ A35 A63 A17✅ A1✅ | 线刷无超时、fastbootd 瞬态致命化、篡改退出无界、root_ota 假忙活锁（根因 A32）、下载零超时×2、pinset 自砖、文案失真、push/dd 超时、root_ota_check 持租约（⑬ 独立确证） | 中（A2/A7/A13/A17/A1 已修；A32+A63 超时接线同族待批；A33/A34 网络超时✅ 09-17；A35 pinset） |
| 真源矛盾（同物两制） | A6✅ A14 A16 A27✅ A36 A37 A39 A53 A54 A56 | 退出码扫描两制、busy 判定两制、mirror_stop 全局屏障、退出码语义倒置、心跳 409/410、validate_token 折叠、文案函数错用、脱敏三真源、分区名校验绕过、Waiting 死状态 | 小-中（A6/A27 已修；多为收敛到单一真源） |
| 日志/遥测完整性 | A5 A10✅ A11✅ A12✅ A40 | 快照回退竞态、usage 串台、清理失败误标、preflight 不进日志（大面积） | 小（A10/A11/A12 quick_flash 侧已修；A5/A40 其余入口待批） |
| 用户体验/资源 | A8 A9 A15 A18 A21 A22 A26 A28-A31 A38 A41-A52 A64 | inspect 对称、时钟回拨、版本缓存、-T 形态对撞、ROOT 留档、staging 泄漏、UAC 挂起、crash 钩子死链等 | 小-中（未动；A64 是 P0 功能级死链，建议优先批） |
| 其余 | A3 A4 A24✅ A25 A59 | 发现链超时（A23 根因家族）、current_gate 单槽覆盖（已随 TOCTOU 修复部分处理）、scrcpy 孤儿进程、A59 随 A57 一并解决 | 中 |

建议批次：①变砖级（A19 A20）→ ②P0 锁死家族（A57 A58 A61——同一次 mirror.rs 结构改动，连带 A59/A60）→ ③挂死家族（A2/A7/A13/A23/A32 超时接线同族，A33/A34 网络超时，A35 pinset）→ ④真源收敛（A6/A14/A16/A27/A36/A37/A53/A60 等多为删重复/统一函数）→ ⑤日志完整性（A5/A10/A11/A12/A40 同族）→ ⑥其余按组。A24 已随对抗审查落地（D-8）。关联澄清项需先拍板再修的：C-25/C-31/C-32/C-34（影响 A20/A19 家族修法）、C-49（影响 B34 修法方向）。

---

## A. 建议批准修复（P0/P1，有明确修法）

### A1. ⚠️P0 `command_timeout` 缺 `push`/`dd` 规则 —— ADB Root 大分区写入中途被 60 秒强杀，super 半写不可开机
- 位置：`command_timeout.rs:31-54`（`for_command` 只匹配裸参数 `"flash" | "dd"`）；消费链 `quick_flash.rs:106-120`。
- 场景：ADB Root 通道刷 5–15GB 大分区：`adb push`（暂存上传）与 `su -c 'dd if=… of=…'`（`dd` 埋在引号串内不命中 FLASH 档）全落 CONTROL 60 秒——push 超 60s 即被 `terminate_process_tree` 中途强杀，staging 截断；设备端 dd 写 super 分区被 host 侧超时终止时，分区半旧半新即**不可开机**。两份报告（quick_flash+windows）独立确认，windows 报告定性为伤设备级。
- C# 对照：`AdbRootTransferRunner.cs` 的 push **无墙钟超时**（仅取消）；且 root.rs:1326 用 ROOT_PATCH 5 分钟档作 fallback——架构明知传输需要长预算，quick_flash 的 CONTROL fallback 自相矛盾。
- 修法：`for_command` 增加识别（args 含 `push`、或 `shell`+`su` 组合且串含 `dd`/`blkdiscard` → TRANSFER/FLASH 档）；分类器看不到引号内命令是结构性缺陷，需按 program+形态分类。

### A2. ⚠️P0 线刷（safe_flash）整条执行链**完全无超时**——违反自家「任何命令不允许无限挂起」不变量
- 位置：`nwflash-application/src/safe_flash.rs:472-499`（run_required）、`:504-527`（run_partition_flash）；根因 `nwflash-windows/src/process.rs:645-652`（`SystemCancellableProcessExecutor::run` → 超时恒 None）。
- 场景：线刷的重启、getvar 预检、全部 `fastboot flash`、set_active、reboot 均无上限。USB 半断开致 fastboot.exe 卡死 → 操作门永久占用、UI 永停「执行中」。取消可传播（用户手点能解），无人值守无解。C# 对照：`FastbootCliRunner.cs:225-228` 对 flash 用 IO 无进展 600s、短命令墙钟 20/60s，**没有任何 fastboot 命令无界**。
- 修法：`SafeFlashExecutionService` 改经已有但未接线的 `run_with_timeout`（process.rs:632-662），按 `for_command` 分级传超时。

### A3. P1 ADB 设备发现链无超时无取消 —— 卡死的 adb/fastboot 服务器永久冻结设备检测
- 位置：`device.rs:604`（手动刷新 run_command 无界）、`:625-637`（心跳路径）、`:644-651`（getprop/battery）、`device_identity.rs:28`、`quick_flash.rs:415`。
- 场景：fastboot getvar 数据线半断时出了名会挂死（C# 为此设 20s ProbeTimeout）——Rust 版无界 → spawn_blocking 永久阻塞 → 设备检测冻结、序列号绑定失真，只能重启应用。
- 修法：发现链全改 `run_command_with_cancel` + PROBE 档（C# 语义 20s），token 传入 discovery。

### A4. P1 `current_gate` 单槽覆盖 —— 共享操作注册即顶掉设备操作的取消凭据，Stop 对刷写永久失效
- 位置：`operation_coordinator.rs:838-844`（无条件覆盖）、`:1006-1012`、`:1091-1099`。
- 场景：30 分钟级 Flashing 进行中发起一次固件哈希/投屏（共享通道不互斥）——共享操作注册覆盖刷写条目，其结束时 clear_current 拿走条目，刷写的 Stop 变空操作。注册端覆盖与清理端按 id 匹配、注释「可能有并发操作」自相矛盾。
- 修法：current_gate 改 BTreeMap<operation_id, gate>；cancel_current 优先取消设备通道操作。

### A5. P1 快照回退竞态 —— `set_running` spawn 落地 vs 终态 await 直写，旧 id 可覆盖新 id
- 位置：`operation_coordinator.rs:1063-1081`+`:1146-1151` vs `:1086-1089`。
- 场景：操作 A 收尾与操作 B 启动竞争写锁——前端序列变 running(B)→completed(A)→idle，B 运行状态消失；`should_compensate_device_refresh` 被假 idle 边沿误触发。
- 修法：set_running 改 await 直写，或写锁内校验 operation_id 单调性。

### A6. P1 fastboot 退出码 0 + 协议失败扫描只在 quick_flash，线刷完全信任退出码
- 位置：扫描函数 `quick_flash.rs:125-139`（注释明言防「半刷后继续刷下一分区」）；缺失方 `safe_flash.rs:522-526`（`exit_code == 0 → Ok`），run_required 连输出都不保留。
- 场景：fastboot 返回 0 但 stderr 含 `FAILED (remote:…)`——quick_flash 判失败，线刷判成功并继续刷下一分区/set_active/重启。同一 fastboot.exe、同一协议，两通道行为相反。
- 修法：`fastboot_output_reports_failure` 下沉到公共传输层，run_partition_flash/run_required 的 exit==0 分支同样扫描。

### A7. P1 `wait_for_fastbootd` 把一切瞬态探测失败当致命 —— 180s 重试循环形同虚设
- 位置：`safe_flash.rs:543-570`。`?` 让一次 `fastboot devices` 抖动或 `getvar is-userspace` 失败直接炸掉整个线刷；循环重试唯一情形是「计划设备未现身」。
- 对照同仓：quick_flash.rs:593-611 等价循环把 unknown variable 按「还不是 fastbootd 继续等」处理、传输失败仅记录。两个都声称对齐 C# `MatchesTargetAsync` 的实现行为相反。
- 修法：两处 `?` 改「记录+本轮作废+继续等」，仅截止后报 DeviceUnavailable。

### A8. P1 固件提取元数据探测（inspect_payload）无超时无无进展监视，与提取路径不对称
- 位置：`firmware_extract.rs:72-77`（None 超时）对照 `:455-482`（提取有 120s PayloadProgressWatch）。
- 场景：payload 源远程/半开时 `payload_dumper -l` 元数据阶段挂死——同一工具、同一网络风险面，两个入口不同生死。
- 修法：inspect 路径套用同一 PayloadProgressWatch 或至少墙钟超时。

### A9. P1 文件删除/列表与分区备份 dd 传 `None` 超时（可取消但违反不变量）
- 位置：`files.rs:567`（files_delete）、`:689`（files_list）、`partitions.rs:778-783`（备份 dd 回读大分区唯一靠取消兜底）。
- 矛盾：`files.rs:34-37` 定义了 FILE_TRANSFER_TIMEOUT 却没接这两条路径，其注释自称「文件命令目前显式传入自有超时」——与实现矛盾。
- 修法：files_delete/files_list 传 CONTROL/PROBE；备份 dd 传 TRANSFER。

### A10. P1 usage 明细跨操作串台 —— `details.clear()` + 收尾全量 clone
- 位置：`operation_coordinator.rs:893-895`、`:443`、`:983-988`；log_with_detail :1117 唯一追加点。
- 场景：刷写进行中发起固件哈希 → 哈希 clear 掉刷写明细，两侧 usage 日志 details 交叉/丢失。仅影响遥测。
- 修法：operation_details 改 BTreeMap<operation_id, Vec> 分桶（改动封闭在协调器内，无外部引用）。

### A11. P1 暂存清理失败误标 Failed ——「已刷成功」被标成失败
- 位置：`quick_flash.rs:1677-1699`。
- 修法：清理失败降级 report_warning，任务仍记 Succeeded。

### A12. P1 quick_flash 预检早期失败不进操作日志
- 位置：`quick_flash.rs:1240`、`:223-226`、`:718-736`。对比 partitions.rs:487 已修同类。
- 修法：早期 Err 分支统一接 `report_preflight_failure`。

### A13. P1 ImmediateTamper「立即退出」先无界等待在途操作——卡住的刷写让篡改退出无限期推迟
- 位置：`exit_supervisor.rs:19`（TAMPER_CLOSEOUT_DEADLINE=750ms）vs `:398-402`（`wait_until_idle().await` **无超时**先于 750ms 窗口）。
- 场景：VMP 完整性失败应立即退出，但实现先等在途操作（fastboot 30 分钟兜底）结束才开始 750ms 收编——「立即」语义落空且 terminate 不被调用。同一 goodbye 两个预算：正常登出 3s vs 监督路径 750ms。
- 修法：ImmediateTamper 类跳过等待（或以 750ms 为上限的有界等待），goodbye 预算统一。

### A14. P1 前端 `operation:snapshot.is_busy` 仍按快照 kind 推导，与许可真源漂移
- 位置：`lib.rs:1029-1038`（`matches!(snapshot.kind, Discovering|…|Mirroring)`）。
- 场景：授权等待窗口内许可已忙（心跳已不计数），但快照 kind 仍 Idle → 前端 is_busy=false、`should_compensate_device_refresh` 用该值判定——补偿刷新可能在授权窗口误触发，前端停止按钮状态与真值漂移。
- 修法：广播循环改为附带读 `coordinator.is_busy()`（注意与快照事件的时序竞争，需设计）。
- 关联澄清：C-10（忙的三套表示）。

### A15. P1 时钟回拨防护只接本地准入，心跳租约校验裸用 unix_now()
- 位置：`lib.rs:198-201`（锚点防「拨钟延长租约 expires_at」）vs `auth.rs:219-224`（心跳租约判定裸 `unix_now()`）。
- 场景：断网+拨钟后心跳路径仍接受过期租约（会话看似健康），本地高危操作却被锚点拒绝——同一租约两个消费方一个防护一个不防护。
- 修法：心跳租约时间判定同样过锚点。

### A16. P1 mirror_stop 是全局空闲屏障——「结束投屏」挂到固件下载/哈希结束
- 位置：`mirror.rs:407-416`。stop 后 `wait_until_idle()` 要设备许可+全部 64 个共享许可。
- 修法：只等自己的 Mirroring 操作（按 operation_id 定点等待），或接受现状并在前端禁用期间给提示（产品决策）。

### A17. P2 fastbootd 等待超时文案按 360s、实际窗口 180s
- 位置：`safe_flash.rs:137-139`（注释「360×500ms=180s」）vs `:580-585`（`as_secs().max(1)` 把 500ms 算成 1s → 文案 360 秒）。实际窗口 180s（对齐 C#），报错文案夸大一倍。
- 修法：文案按 `attempts × interval` 真实换算。

### A18. P2 VersionClient 把传输失败缓存为 ALLOW_ALL 永久生效
- 位置：`version_client.rs:74-103`。启动时一次网络抖动 = 本次运行永不提示更新。
- 修法：失败兜底不缓存，或缓存带有效期（产品决策）。

### A19. ⚠️P1（变砖级）ROOT 全自动流程各阶段独立重读设备序列号，跨阶段无设备绑定
- 位置：`root.rs:2122-2125`（每轮循环重读 `active_adb_serial()`）。
- 场景：全自动流程跨数分钟，A 机拔出、B 机插入 → 修补阶段在 A、刷写阶段取到 B 的 serial，把 A 固件修补的 boot 镜像刷进 B。同模块手动修补刷写路径反而有严格绑定（verify_execution_plan_device）——同一模块两种强度。C# 真源全程用 session.Serial 单事务贯穿（RootViewModel.cs:685-702）。
- 修法：take_automatic_with_lease 时捕获 serial，每阶段开头比对，不一致中止。

### A20. ⚠️P1（变砖级）线刷预检→执行两段式之间无设备一致性校验，且被测试明文固化为"接受换设备"
- 位置：`safe_flash.rs`（tauri）`:1167-1175`（执行时重解析设备，prepared.options.serial 不参与校验）；测试 `:1645-1703` 注释「command execution must not reject a changed preflight serial」。
- 场景：在线固件源按 A 机的 PD/版本匹配下载解包，确认弹窗未点期间换 B 机（fastboot）→ A 机整包固件刷进 B。prepared entry 不过期，窗口可达数小时。quick_flash 对同一情形明确判死（"连接设备已变化"）；C# prepare 与刷写在同一操作内连续完成，无此间隙——重写引入而非真源固有。
- 修法：执行时当前 serial 与预检 serial 不一致（至少对在线源）要求重新预检。
- 关联澄清：C-25。

### A21. P1 vendor_boot 修补全链脚本的 adb 传输形态与同文件注释宣称的"实测必失败"直接对撞
- 位置：注释 `root.rs:1022-1024`（断言 `shell -T sh -c <脚本>` 形式实测语法错误退出 1）vs 实现 `:1121-1238`（unpack/模块更新/repack/模块列表全部用该形态）。
- 场景：官方 KernelSU vendor_boot 修补链（全自动或手动）在任何真机上的可用性存疑；现有测试只断言命令构造形态，从未验证传输语义。C# 真源两条链全部用单参数整段脚本。
- 修法：统一到注释宣称可用的形态（adb shell 整段脚本），或修正注释附真机实测记录——两处必须一致。先真机验证再定方向。

### A22. P1 ROOT 修补产物桌面留档契约断裂——导出命令零调用，产物随会话失效被删
- 位置：`root.rs:2038-2066`（root_export_patched_artifact 带导出意图的文档注释）；前端 RootPage 全文无该 invoke。
- 场景：C# 每次修补成功自动导出桌面（用户唯一救砖留档）；Rust 实现了命令但无 UI 入口，产物只在临时 staging，会话失效/重新修补时被删——与注释承诺"始终能拿到"矛盾。
- 修法：修补成功后自动导出（对齐 C#），或 RootPage 加导出按钮。接线时连带修 P2 的路径透出问题（root.rs:2057 error.to_string() 透出 staging 全路径）与 OneDrive 重定向（:2061）。

### A23. P1（组合）root_ota_check 持全套 idle 租约横跨无超时 adb 读 × A3 —— 假忙活锁把「可崩溃恢复」变「永久卡死」
- 位置：`root_ota.rs:279`（租约横跨 adb 读+网络 30s 查询）；根因 A3（发现链无超时）。
- 场景（对抗审查发现）：用户点「检测服务器固件」→ adb 挂死 → 租约持有无上界 + is_busy 恒真 → 心跳空闲退出被**无限推迟**（旧代码 10 连败 force_exit 反而「以死解锁」，新代码 busy=true 使计数归零）→ 应用无限期假忙，网络同时断时永不退出需手动杀进程。同窗口内自动刷新静默冻结、所有操作 InProgress。
- 修法：修 A3 本体（OTA 检测链 adb 读加超时）；或 root_ota_check 分阶段取/放租约。**根因是 A3，此项随 A3 一起批。**
- 关联澄清：C-29（租约持有范围是否有意）、C-30（busy 是否需要看门狗）。

### A24. P2（已随对抗审查落地闭合，见 D-8）report_preflight_failure 的 check→write TOCTOU
- 原：`is_busy()` 读 false 与终态写入之间存在微秒窗口，新操作可插入并被失败快照砸掉 Running 快照。
- **已修**：改用 `try_acquire_idle` 原子守卫（租约在手期间任何 run 被拒，不可能交错）——对抗审查判定「收窄未闭合」，已闭合。

### A25. P1 忙时点 X 留下孤儿 scrcpy 进程/窗口 —— mirror 收尾只挂在 session_stop/退出监督器，X 直接退出两者都不经过
- 位置：`App.tsx:477-520`（closeWindow 忙时静默吞 session_stop 失败直接关窗）+ `lib.rs:145-147`（注释自证 scrcpy 残留窗口/进程是缺陷）。
- 场景：投屏在跑（共享通道）时点 X：`try_acquire_idle` 需要全部 host 许可而失败 → 直接关窗 → 进程退出。Windows 无 Job Object（全仓确认无 AssignProcessToJobObject），scrcpy 存活残留，孤儿窗口留到用户手杀。`stale_pids` 补杀随进程死亡失效。handoff §1.3 只论证了 fastboot/adb「自然跑完更安全」，未覆盖常驻型 scrcpy。
- 修法：`run_app` 加 `CloseRequested`（不 prevent）或 `RunEvent::ExitRequested` 钩子，≤1s 同步收尾（mirror stop + 顺带 owned staging 清理，见 A26），不改变「点 X 一律直接退出」的决策语义。

### A26. P2 忙时点 X 跳过全部后端收尾 —— owned staging 目录泄漏
- 位置：`lib.rs:149-159`（invalidate 清固件暂存/ROOT 修补暂存/线刷 staging）——只有 session_stop/supervisor 触达。
- 场景：忙时 X → revoke 不运行 → 各 owned staging 目录永久留盘。usage 日志有持久化 spool 无丢失（已核实），服务器侧靠租约超时（已接受）。
- 修法：与 A25 同一个退出钩子顺带删除 owned staging（需不依赖 idle 租约的同步清理入口）。

### A27. P2 退出码语义倒置：优雅退出=70，即杀=0
- 位置：`exit_supervisor.rs:20/:503`（监督器所有终局恒传 70，含 Delayed 优雅路径）vs `lib.rs:947`（心跳即杀通道传 0）；文档 vmp.rs:108 写「正常收尾传 0」。
- 影响：发布脚本/运维按退出码区分篡改与正常退出时被误导。
- 修法：Delayed 优雅路径传 0，仅 ImmediateTamper 传 70；至少先改文档对齐现状。

### A28. P2 驱动安装提权进程无超时不响应取消 —— UAC 弹窗没人点就无限挂起
- 位置：`driver.rs:1090-1115`（run_elevated_process 等待循环 WAIT_TIMEOUT→continue 永不退出、不查取消）。
- 场景：用户不响应 UAC / pnputil 挂死 → 「安装 USB 驱动」共享操作无限挂起，operation_cancel 令牌进不去 WaitForSingleObject 循环。
- 修法：等待循环加截止（如 5min）+ 每次迭代检查取消，超时/取消时 TerminateProcess 收尾。

### A29. P2 协调器毒锁/派发恐慌后应用变「僵尸」—— fail-closed 但无人请求退出
- 位置：`operation_coordinator.rs:122-134`（Mutex 中毒恢复置 Terminating+disposed）、`:159-167`（DispatchPanicked 同置）。
- 场景：恢复后一切操作报「应用正在终止」，但进程继续活着——一切操作失败、无提示、不自愈的僵尸窗口。
- 修法：毒锁恢复/DispatchPanicked 处通过 supervisor 请求 immediate 退出。

### A30. P2 10 个已注册 IPC 命令无前端调用方（死注册面）
- 位置：`lib.rs:2265-2293`（firmware_inspect_payload_local、line_flash 系、quick_flash_prepare 系、execute 系、root_export_patched_artifact 等）。
- 仍受权限门保护无越权风险，纯攻击面/维护面。修法：确认无外部契约依赖后删除或显式 `#[allow(dead_code)]` 标注兼容缝。

### A31. P2 getvar 双档位不一致（15s vs 20s）
- 位置：`quick_flash.rs:383-397`（read_fastboot_variable → PROBE=15s）vs `command_timeout.rs:15`（GETVAR=20s 对齐 C# ProbeTimeout）；另 `:1585` 把 GETVAR 档误用在 ADB shell 命令上。
- 修法：统一走 GETVAR；:1585 归回正确档。

### A32. P1（A23 根因精确定位）root_ota_check 内的 adb getprop 无超时无取消——A3 家族在生产唯一的必经实例
- 位置：`device_identity.rs:23-30`（read_online_ota_identity_blocking → run_command，无 timeout 无 cancel 闭包）→ `process.rs:714-716`（timeout None、should_cancel || false）；同一无界 getprop 也出现在 `safe_flash.rs:678-680`（设备独占门内）。
- 场景：设备半断开时点「检测服务器 OTA」→ 持全套 idle 租约 + getprop 永不返回 → is_busy 恒真（心跳退出失效）+ wait_until_idle 锁死退出监督。网络 resolve_rom 本身有界（30s），**无界元凶是它前面的 adb getprop**（网络审计精确定位）。
- 修法：`AdmissionCheckedExecutor` 强制注入 GETVAR 档超时 + should_cancel 接协调器 token——A3/A23 本体。

### A33. P1 remote_firmware 阻塞 HTTP 无任何超时——死 CDN 时固件检测/提取永久挂死且取消不可达（⑨⑩⑪ 三源确认）
- 位置：`remote_firmware.rs:72-77`（default_client 无 timeout/connect_timeout）；`fetch_range/fetch_from`（:292-460）直接阻塞 send()；zip 分支跑在 spawn_blocking 里（不可 abort），取消只在请求开头检查一次。⑪ 补充：取消检查只在每次网络读取**之间**触发——服务器接受 TCP 但永不回包时单次 send() 阻塞在 blocking IO 上，is_canceled 形同虚设；已创建的 partial 文件与阻塞线程继续后台存活。
- 场景：服务器透传的上游 VOTA URL 是「接受 TCP 但不应答」的主机 → firmware_inspect_remote/extract/root_ota_extract_images 永久挂起，host 许可占用，退出监督等死。同 crate firmware_extract.rs:66 探测请求配了 10s 说明作者知道要配——Range 读取链漏配。磁盘 IO 错误也被塞进 Transport 变体（磁盘满与网络故障不可区分，见 C-33）。
- 修法：default_client 加 30s 超时；fetch_from 外包超时+重试（对齐 RemoteAssetDownloader 20s no-progress）。
- **已修（09-17，随用户报障一并落地，见 [2026-09-17-firmware-range-download-fix](2026-09-17-firmware-range-download-fix.md)）**：`default_client` 加 `connect_timeout(20s)` + `timeout(60s)`（阻塞 reqwest 把该超时施加到**每次 `Read::read`**，等价 no-progress 检测）；`fetch_range`/`fetch_from` 改走新的 `fetch_range_bytes()`——4 MiB 逐窗口 + 断流续传 + 零进展退避重试；顺带修正 `validate_range_response` 把 CDN「截短范围」误报成 `RangeUnsupported` 的缺陷。

### A34. P1 safe_flash 在线固件下载（OtaDownloader）零超时零停滞检测（⑨⑪ 双源确认；⑪ 定位为 P1 末位：有取消兜底，仅缺自动中断）
- 位置：`ota_download.rs:697-703`（Client::new() 无超时）；`write_response_to_file` 与分段循环无 no-progress 计时。
- 场景：多 GB OTA 下载 TCP 活着但不传数据时无自动中断（取消可用不至于死锁），设备许可被占、UI 停 0%。与 RemoteAssetDownloader（20s 无进度即断）形成对照。
- 修法：Client::builder().timeout() 或 chunk 循环加 no-progress 超时；分段重试加退避。
- **已修（09-17，见 [2026-09-17-firmware-range-download-fix](2026-09-17-firmware-range-download-fix.md)）**：`build_ota_http_client()` = `connect_timeout(20s)`（不设总超时，多 GB 包会超）；每次 `response.chunk()` 外包 `OTA_DOWNLOAD_STALL_TIMEOUT = 60s`；分段重试改指数退避（400ms→3.2s）且可在退避中取消。

### A35. P1 过期 pinset 缓存令生产启动直接失败——自举恢复路径在构造前不可达（潜伏地雷；⑪ 深化为「两端皆断」）
- 位置：`pinned_tls.rs:505-521`（load_cached 过期即 PinsetTime）→ `:283-314`（new 直接上抛）→ AppState::try_new 失败拒启。⑪ 补充：运行期 pinset 过期后 `ensure_active_at` 同样返回 PinsetTime → 心跳路径 `CloudflareError::Integrity(PinsetTime)` → terminal_classification（session_lifecycle.rs:646）→ **立即完整性退出，无自动恢复**；重启后又是启动 panic——除非手删 `%LOCALAPPDATA%\VivoKsu\nwflash-api-pinset.json`。
- 场景：当前生产无 refresh_pinset 调用方（仅 tests + debug-only `CloudflareClient::refresh_pinset` 也无人调用）故是**潜伏地雷**；一旦按注释「Task 8 release maintenance」启用刷新，任何离线 >7 天再启动的机器直接被拒启——而 bootstrap 客户端（内嵌 pins 降级自举）明明建好了，却死在构造函数走不到它。测试 `expired_dynamic_pinset_can_refresh_only_through_embedded_bootstrap_pins` 专门验证过恢复可行，生产无触发点。另 PinsetCache 文件 IO 错误（Windows AV/索引服务短暂锁文件）同样直接拒绝启动。
- 修法：load_cached 对 PinsetTime/PinsetRollback 降级为「丢弃缓存+内嵌 pins 构造+标记 validity=None」，恢复交给既有 bootstrap 分支；`ensure_active_at` 返回 PinsetTime 的调用点（心跳/登录前置）接一次 refresh_pinset 重试；其余校验失败仍 fail-closed。

### A36. P2 心跳把 409/410 当瞬时静默重试——与服务端注释声明的契约相反
- 位置：`session_lifecycle.rs:505-509`（ApiError 全归 transient）；服务端 `cloudflare/src/index.ts:700-704` 注释：409=疑似篡改客户端应强退、410=会话过期应回登录窗——但服务端 409 时并不设置 force_exit_at。
- 后果被两层兜底收窄（空闲 50s 十连败退、忙时靠 120s 租约 TTL），但 410「会话已亡重试无意义」与 409「安全事件」与 429/5xx「该重试」被折叠成一类；410 场景文案「连续心跳失败」误导。
- 修法：410 独立分类触发回登录 UI；409 与服务端约定（服务端补 force_exit_at 或客户端走 Integrity 通道）。关联澄清 C-34。

### A37. P2 validate_token 把网络抖动折叠成「未登录」——UI/Rust 会话视图分裂
- 位置：`auth.rs:151-160`（Err(_) => Ok(None)）。
- 场景：会话恢复时一次网络抖动 → UI 判「未登录」，Rust 侧 token/心跳/租约照常存活；用户被误导重登（全量换代收敛）或直接关软件。
- 修法：Transport/InvalidResponse 三态或保守 propagate，仅 401/410 映射未登录。

### A38. P2 V1 usage 队列头部阻塞——Permanent/Transient 二分无消费方且 429 误归 Permanent
- 位置：`usage_reporter.rs:158-206`（flush 任一 is_err 整批保留——失败批永久占队头，后续条目含其他账号永远发不出）；`tauri/usage_reporter.rs:70-73`（429 归 Permanent）。
- 场景：一批被 400 拒后每 30s 永久重试同一批，spool 无上限增长。二分本意就是死信语义——当前是死代码。
- 修法：Permanent 增加丢弃/死信；429 移出 Permanent。

### A39. P2 root_ota_check 失败日志用错消息函数——404 显示兜底文案而非 ROM 专属提示
- 位置：`root_ota.rs:384-386`（user_message）vs `safe_flash.rs:686-692`（正确用 rom_lookup_message）；api_client.rs:54-77 的 user_message 无 404 分支。
- 修法：换 rom_lookup_message()（一行）。

### A40. P1 preflight 失败不进操作日志——除 partitions.rs 外全部入口未接入已修模式（大面积）
- 位置：`files.rs:669/721/733-741/765-770/788`（files_list 最常见：设备断开时日志区完全不可见）、`resources.rs:80`、`drivers.rs:26-27`、`firmware.rs:1340-1342/1366-1371/1456-1458/2057`——全部在 run_shared_async/run_async 之前返回，不经终态日志路径。
- 修法：按 partitions.rs:482-489 模式在各早退分支补 `report_preflight_failure(title, &message).await`。与 A12（quick_flash 入口）合并实施。

### A41. P2 resource_inventory 同步命令全包 SHA-256——页面打开可冻结主线程数秒
- 位置：`resources.rs:190-202`（同步命令）+ `scrcpy_provisioner.rs:213-240`（命中 exe 即对包内每文件串行哈希）。
- 修法：改 async + spawn_blocking，或缓存（指纹+mtime）。

### A42. P2 resource_install 全链下载无进度回调——UI 只有阶段文案，用户以为死机
- 位置：`resources.rs:105/117/132/147` 全部 `ensure_installed(&cancellation, None)`。
- 修法：接 ProgressSink 转发 context.report_progress（files.rs:635-645 有范式）。

### A43. P2 提取链无磁盘空间预检，瞬时占用最高 3× 镜像体积
- 位置：`firmware_extract.rs:427-449`（暂存 1×）+ publish（输出 1×）+ `firmware.rs:488` 快照复制（1×）。失败清理完备（已核实），但 C 盘紧张时用户白等。
- 修法：选中分区后按 total_bytes × 3 检查目标盘+temp 盘剩余空间再进协调器。

### A44. P2 FirmwareExtractionRuntime::replace 同步全量复制——进度已 100% 但命令卡数十秒不可取消
- 位置：`firmware.rs:488`（tokio worker 上同步复制，发生在协调器 await 完成、进度报 1.0 之后）；remote_image_entries 不过滤分区大小（super.img 也入列）。
- 修法：spawn_blocking + 快照进度汇报；或仅对受控四分区快照、其余直接引用输出路径。

### A45. P2 本地固件 inspect→extract 按 index 重列的 TOCTOU + 远程 zip 无摘要绑定
- 位置：`firmware_extract.rs:256-319/627-762`（重 list 后按 index 取，无交叉校验）；`remote_firmware.rs:180-290`（expected_size 来自提取时刻新读的中央目录，与 inspect 时刻无绑定）。
- 缓解已核实：payload 路径有 expected-size 硬校验；分区映射按文件名派生不会刷错分区；DTO 回显提取名。剩余风险是 index 指向被替换后的另一条目/会话期间远程内容被换。
- 修法：extract 时用选中条目 (name, size) 与重列结果比对，不一致报"固件包已变化，请重新读取"（远程路径已有同款防御可抄）。

### A46. P2 固件进度事件无操作区分符——共享通道并发操作进度串扰
- 位置：`firmware.rs:36-50`（FirmwareProgressDto 无 operation_id）；`lib.rs:1120-1124` 单一 sink 全局广播。
- 场景：两个并发提取的进度事件交错到同一监听器，进度条来回跳。与 A4（current_gate 单槽）同根因不同通道。
- 修法：DTO 加 operation_id，前端按 id 分流。

### A47. P2 驱动安装失败路径暂存清理被吞 + LocalProgressPoller 无 Drop
- 位置：`driver.rs:139-144`（无论成败 `let _ = cleanup`——失败清理也静默，%TEMP% 残留）；`files.rs:374-420`（poller 无 Drop，run_process panic 解栈时线程每 250ms 向已完结操作报进度）。
- 修法：清理失败 report_warning 留痕；poller 加 Drop 置 stop。

### A48. P2 payload_dumper 暂存无 stale 清理（scrcpy 有、payload 没有）
- 位置：`payload_provisioner.rs:144-185` 对照 `scrcpy_provisioner.rs:281-301`。
- 修法：ensure_installed 加载时清扫孤儿目录。

### A49. P2 resource_downloader 提交用替换式 rename——与 files.rs 的 no-replace 契约不一致
- 位置：`resource_downloader.rs:357-367`（检查后 rename，Windows rename=REPLACE_EXISTING，窗口内出现的目标被静默覆盖）；对照 files.rs:304-321 用 promote_without_replace。资源经 SHA-256 验证后提交，完整性无损，仅多实例竞态浪费。
- 修法：复用 promote_without_replace。

### A50. P2 上传到设备根目录 "/" 必失败且报"清理未确认"
- 位置：`files.rs:225-235`（"/" → temp 落 `/.nwflash-upload-*.partial`，无权写 /，push 失败 + cleanup 失败 → cleanup_failure 覆盖真实原因）。
- 修法：目录为 / 时直接拒绝或回落 /data/local/tmp。

### A51. P2 operations.log 每条 open/append + 同步 I/O 在 async worker 上；轮转失败无界增长
- 位置：`operation_log.rs:67-91/141-166`（rotate remove(.1) 失败则 rename 失败 → 超 2MB 继续追加）；写入方在 tokio worker 同步调用。
- 崩溃恢复已核实无问题（torn line 跳过）。
- 修法：持久化句柄+BufWriter 或 spawn_blocking；rotate 失败降级截断。

### A52. P2 usage 记录未登录时静默丢弃（设计如此，仅记录）
- 位置：`usage_reporter.rs:336-343`（credential=None 不上报）。V1 桥接层语义。
- ⑪ 复核注：usage_reporter 模块头已声明该行为为 C# 对齐的过渡设计（V1 退休桥），保持。

### A53. P2（⑪）quick_flash 命令层「公开安全文案」直接透传原始明细——脱敏三真源并存
- 位置：`operation_coordinator.rs:380-392`（public_operation_failure_message，变体→通用文案）；`commands/device.rs:46-56`（DeviceDiscoveryFailure::public_message）；`commands/quick_flash.rs:158-167`（public_failure_message——**对 ExternalTool/Internal/InvalidOperation/InvalidInput/DeviceUnavailable 直接 message.clone() 返回原始串**；锚点复核 2026-09-12 确认，变体名为 DeviceUnavailable 非 DeviceCancelled）。
- 场景：快速刷写失败弹窗/前端 toast 收到 fastboot 原始 stderr（含设备序列号/本地路径）。协调器终态快照会再脱敏，但命令返回值（如 device.rs:884 `result_to_domain_error(error).to_string()`）不再过 public_operation_failure_message——`operation_failure_detail`（coordinator:305）注释明确「页面只展示通用文案」，quick_flash 版与之矛盾。
- 修法：将 public_operation_failure_message 上提为 DomainError 固有方法（domain 或 application 单一定义），quick_flash 复用；命令层返回前统一过该方法。关联 C-47（同名职责函数、语义互斥）。

### A54. P2（⑪）safe_flash/quick_flash 直接构造 PartitionExecutionPlan 绕过 validate_partition_name——防御纵深缺口
- 位置：`application/safe_flash.rs:813-887`（build_plan 直接 push PartitionTask，仅查 trim 非空）；`commands/quick_flash.rs:249-261, 297-313`（直接构造）。对照 `domain/partition.rs:308-321` validate_partition_name 的存在理由就是「names reach a fastboot argument」，且 PartitionExecutionPlanBuilder（partition_workspace.rs 全路径）确实逐名校验。
- 场景：safe_flash 的 source.partition_name 源自固件包 zip 条目 file_stem（list_single_image）与前端输入，未走字符集/长度校验即成为 fastboot argv。argv 传递无 shell 注入面、设备侧会拒绝未知分区，故 P2；但这是域自身不变式在同 crate 家族内的选择性执行。
- 修法：safe_flash build_plan 内对每个 target 调 validate_partition_name；或把 PartitionTask 构造收进域内非 pub 字段/构造函数。

### A55. P2（⑪）application 依赖 zip = "9.0.0-pre3" 预发布版用于安全刷写生产路径
- 位置：`crates/nwflash-application/Cargo.toml`（dependencies）；`application/safe_flash.rs:15`。
- 场景：VIVO 线刷（安全刷写）包解析走 zip 9.0.0-pre3 预发布；infrastructure 用稳定 4.2.0。同仓两个 zip 大版本并存，预发布 API/行为变更风险直接落在刷机路径。
- 修法：统一到稳定版（升级 infrastructure 或降级 application；迁移期间锁版本+CI 校验）。

### A56. P2（⑪）PartitionTaskState::Waiting 全仓无构造点——英文状态词「Waiting」直进中文 UI
- 位置：`domain/partition.rs:44-50`。后端仅 Running/Succeeded/Failed/Canceled 有构造（quick_flash.rs:178-207 等）；前端 LineFlashPage.tsx:280 直接渲染原始 state 字符串，「Waiting」无任何 producer。
- 缺陷：状态机声明了从未使用的初始态——要么实现排队预览（构造 Waiting），要么删变体（破坏前端字符串契约前需同步 ipc-events.ts）。另 LineFlashPage 建议做中文映射。
- 修法：与 A30（前端死注册面）一并处理时定夺：删变体+改前端渲染映射，或补排队预览。

### A57. ⚠️P0（⑫）投屏会话期间全局 busy 恒真——登出被双重封死、安全退出被阻塞
- 位置：`mirror.rs:246-332`（start_plan 把整个投屏生命周期包成一个 Mirroring 共享操作，持 1 枚 host_lane 许可直到 scrcpy 死且监督循环退出）；`operation_coordinator.rs:677-680`（is_busy 许可派生：投屏数小时=busy 恒真数小时）；`session.rs:30-34`（session_stop→try_acquire_idle 因投屏持许可返回 InProgress）；前端 `App.tsx:876`（登出按钮 disabled）。C# 真源：MirrorService 完全不经过 OperationCoordinator（IsBusy=state.Kind!=Idle，镜像从不触碰），登出时顺手 StopAsync。
- 场景：用户开着投屏想退出登录换账号——前端按钮禁用；绕过前端后端也拒；心跳终态清理、退出监督 wait_until_idle 同样拿不到空闲屏障；长投屏同时抑制心跳空闲终态（与 C# 语义相反）。注意：结构缺陷先于 is_busy 修复存在（旧 AtomicBool 同样置真），修复使其确定性暴露。
- 修法（报告推荐方案 A）：投屏不持协调器许可横跨会话——start_plan 启动成功发信号后即返回，监督循环脱离操作许可自持 runtime 状态；取消/退出走 stop_notify + admission 轮询（现成机制，把 admission_state 检查移到独立 spawn 循环）。同时解除 P1-1（投屏抑制设备刷新）与「长投屏抑制心跳终态」的连带。方案 B（对 Mirroring 特赦 busy）不可取——破坏空闲语义。

### A58. ⚠️P0（⑫）投屏断流恢复语义与 C# 相反——设备拔插/重启烧光重启额度并永久关闭自动投屏
- 位置：`mirror.rs:298-321`（重启块只查 should_auto_restart_after_exit=自动开+非主动停，**不复查设备在位**；runtime.start 复用启动时冻结的旧 serial）；`:140-147`（abandon 把 auto_mirror_enabled **持久置 false**）。C# 真源 MirrorService.cs:193-206 有 AdbConnected 闸门：设备不在不重启、不烧计数。
- 场景：自动投屏开着 → 手机重启（30-60s 无 ADB）→ scrcpy 对空 serial 连发失败 → 3 次即 abandon，用户开关被静默篡改为关——设备回来后自动投屏已死。功能的存在意义（会话意外关闭自动恢复）被倒置。
- 修法：重启前复查 active_adb_serial，serial 变化时用 build_start_plan 重建（对齐 C# ReconcileAsync）；设备不在位时挂起等待（不烧计数）；abandon 不持久关用户开关（改本地抑制+用户可见状态）。

### A59. P1（⑫，A57 使能缺陷）投屏期间自动设备刷新被永久跳过——快照停死
- 位置：`device.rs:411-416`（`is_busy() → Flashing → device_refresh_block_reason 拒绝`）。A57 使投屏全程 busy → 3s 心跳刷新每轮跳过 → 拔插后 active_adb_serial 停留在陈旧的 AdbConnected——即使修了 A58 重启闸门也拿不到新设备身份。C# 镜像不置 busy，投屏期间刷新照常。
- 修法：随 A57 方案 A 一并解决；手动刷新不受影响（只对 Flashing 拒绝）是对的一侧。

### A60. P1（⑫）投屏自动重启计数三处复位全缺——重开自动后一次机会都不给
- 位置：`mirror.rs:79-84`（begin_manual_start 只清 deliberate_stop）、`:183-198`（stop 不清计数）、`:86-102`（set_auto_enabled(true) 不清计数）；唯一复位点 `:320`。C# 真源 StartAsync(:58)/StopAsync(:71)/ClearDeliberateStop(:65) 三处都复位 consecutiveRestartFailures。
- 场景：abandon 后计数停 3；用户手动开投屏正常；重开自动开关 → 计数仍 3 → scrcpy 第一次退出即 note_restart_failure 返回 4≥3 立即 abandon——正常使用被上次的残值误杀。
- 修法：三处调用 reset_restart_failures()。

### A61. P1（⑫）scrcpy failover 下载传入永不取消的新 CancellationToken——下载挂死即全局锁死
- 位置：`mirror.rs:219-227`（`.ensure_installed(&CancellationToken::new(), None)`——新 token 无人持有）；操作体真实 cancellation 闭包参数没有管道进 plan_provision（future 在 `:377-388` 操作外构造）。
- 场景：网络挂起下载卡死 → Mirroring 操作永不结束 → busy 恒真 → 登出/退出/取消全部无效（与 A57 叠加成永久锁死）；用户点 cancel_current 对该下载无感。
- 修法：CancellationToken 作参数传入 start_plan → plan_provision，ensure_installed(&cancellation, None)。

### A62. P1（⑫）驱动安装后复检时序丢失 + DTO 三旗标被前端全数忽略——滞后窗口误判失败
- 位置：`drivers.rs:64-74`（安装结束立即 detect_drivers——DriverStore 登记可滞后）；`App.tsx:623-634`、`SoftwarePage.tsx:110-122`（两个调用点都不读返回的 adb/fastboot/mediatek_driver_installed 旗标；成功即关弹窗/立即复检）。C# DriverReminderWindow.xaml.cs:115-136 专门做了「exit 0 但尚未登记」的分支文案。
- 场景：用户在滞后窗口内看到「未安装」，误判安装失败。
- 修法：安装成功后延迟/重试复检，或前端消费 DTO 旗标给出「已完成但系统尚未登记，稍后重新检测」中间态。

### A63. P1（⑬）root_ota_check 持全量 idle 租约横跨无超时 getprop + 网络查询——协调员点名评估的「未发现持租约路径」，确证存在
- 位置：`root_ota.rs:279`（try_acquire_idle 拿全部 1+64 许可，持有至函数末尾 424 行）、`:346`（getprop）、`:376`（resolve_rom 30s 有界）；根因 `device_identity.rs:23-30`（getprop 走 run_command，timeout=None 无取消）。`safe_flash.rs:678` 同一无界 getprop 在 Flashing 门内（挂死时设备通道永久占用且用户取消无效——legacy 路径无 should_cancel 回调）。
- 场景：用户打开/刷新 Root 页（前端每次进页自动触发）→ 检测期间所有操作报 InProgress；adb 卡死 → getprop 永不返回 → 协调器永久忙 → 无法登出、is_busy 恒真、心跳空闲退出被永久抑制——**唯一出路点 X 强退**。与 A23/A32 同族（A32 是本条的发现链根因定位），auth 报告独立确证「root_ota_check 是唯一新增无界路径，finalize/closeout 均已有界」。
- 修法：① getprop 改 run_command_with_timeout（PROBE 15s）+ 取消令牌（随 A32 家族）；② root_ota_check 改 run_shared_async 或把租约收窄到 getprop 窗口、网络查询放租约外（C# 参考云提取跑在操作模型下，可取消可显示）；③ 顺带修 safe_flash:678。
- 关联：A23（组合缺陷）、A32（根因）、C-29（租约范围是否有意）。

### A64. P1（⑬）崩溃上报链路整体死链——panic 钩子装在永不编译的二进制里，crash.log 永不产生
- 位置：`src-tauri/src/main.rs:7-32`（真实入口，**无** install_crash_hook）vs `crates/nwflash-tauri/src/main.rs:41-48,51`（钩子所在）；`nwflash-tauri/Cargo.toml:6`（autobins=false 无 [[bin]]）；`build.rs:11-15`（构建期断言「无 shipping binary」自证死文件）；`crash_uploader.rs:1-8`（读取方自述 P0 功能）。全仓 grep set_hook 仅命中死文件。
- 场景：生产版任意 Rust panic——crash.log 永不写入，start_crash_upload → run_pending_crash_upload 永远读空文件，**整个「崩溃补传（P0 功能）」在产品里惰性空转**，用户崩溃后服务端永远收不到诊断。
- 修法：install_crash_hook 及辅助函数移到真实入口 src-tauri/src/main.rs（或 run_app 开头），删除死掉的 crates/nwflash-tauri/src/main.rs；补启动期冒烟验证。
- 注：与壳层审计的退出码发现相邻但独立。

---

## B. 记录项（P2，修复可选）

- **B1** `partition_operation_error` 把 UserCancelled 改写 ExternalTool（quick_flash.rs:143-156），与 partition_terminal_state 变体判别矛盾——潜伏死分支；删除或改透传。
- **B2** spawn 的 stage/progress 更新可能落在终态之后改写 idle 快照（operation_coordinator.rs:1146-1151）——UI 瞬时怪快照。
- **B3** 双槽确认窗口内 current-slot 可能过期（quick_flash.rs:1163-1183→:1729-1752）——执行时重读复核。
- **B4** `dispose()`/`_operation_task` 全仓无生产调用方（dead code）。
- **B5** run_shared_async 文档未写「共享操作体必须轮询 admission_state」义务——补注释。
- **B6** `TRANSFER` 超时档标注 dead_code「未来重新接线」——与 A1/A9 一并处理。
- **B7** `terminate_process_tree` PID 复用竞态（process.rs:1246-1272，与 C# 同等竞态非回归）——文档标注已知窗口。
- **B8** 超时/取消路径 `TerminationUnconfirmed` 上报不区分实际结果（process.rs:961-972 丢弃 reap 布尔）——与 WaitFailed 路径对齐。
- **B9** detach 的 reader 线程连带 ObservationDispatcher 工作线程存活到孙进程生命周期（process.rs:553-554）——detach 注释补充这层后果。
- **B10** `verify_if_bundled_platform_tool` 用精确 Path 相等（platform_tools.rs:214-224），大小写/斜杠敏感——改 OsStr 忽略大小写+归一化。
- **B11** 完整性缓存指纹可被同尺寸+同 mtime 替换绕过（platform_tools.rs:117-133，已注释声明取舍）——注释标注绕过面。
- **B12** `write_vivo_adb_usb_ids` 追加不保证前置换行（driver.rs:933-943）——写入前检查既有内容尾换行。
- **B13** `validate_adb_staging_path` 缺 `.`/`..` 组件检查（device_transport.rs:361-374，当前路径全内部生成属潜伏面）——补齐与 device_path 对齐。
- **B14** mirror.rs 的同名 `terminate_process_tree` 语义与 process.rs 不同（taskkill 结果被忽略、已退出算失败）——收敛到一处实现。
- **B15** 设备 shell 引号转义两套风格（device_transport.rs:382 `'"'"'` 法 vs file_manager.rs:363 `'\''` 法）——统一公共函数。
- **B16** 子进程输出全按 UTF-8 lossy 解码，zh-CN 本地化报错文本变 U+FFFD（process.rs:1235）——仅显示层 mojibake，C# 显式钉死 UTF-8 属继承假设，记录备查。
- **B17** `ObservationDispatcher::dispatch` 2ms 忙等自旋（process.rs:327-357）——可接受，如需优化改定时轮询。
- **B18** driver.rs:140-144 死代码（计算后即弃的 cleanup）——风格清理。
- **B19** ROOT/safe_flash 前置失败全部不落操作日志（root.rs:1773-2028 多处、safe_flash 预检失败），且 safe_flash prepare 存在「run_async 已记 Success、发布失败页面报错」的日志-用户所见矛盾——按 partitions.rs 模式接 report_preflight_failure。
- **B20** 官方 KSU 手动流程无法修补 init_boot（RootPage 绑死 vendorBoot；后端 manager 字段前端从不传）——补 UI 入口或明确砍掉。
- **B21** 全自动失败后前端镜像选择未重置而后端已消费（RootPage.tsx:369 只清成功分支）——catch 分支同样清空。
- **B22** "已刷入分区数"把清除数据 misc 计入（application/safe_flash.rs:344-356 元组 is_flash=true）——C# 只数 images；wipe 改 is_flash=false 或 DTO 单列。**→ 已修（2026-09-21）：清除数据不再写 misc（改为队列末尾 `fastboot reboot recovery` + 手动清除指引），该计数只数固件分区刷写，见 [清除数据流程](2026-09-21-safe-flash-wipe-data-flow-review.md)。**
- **B23** ROOT 手动修补刷写 prepare 只要求 FastbootConnected、execute 才要求 fastbootd，错误文案不指路（"请 adb reboot fastboot"）。
- **B24** has-slot 瞬态读取失败时 OtherSlot 静默降级为原槽刷写，且丢失 C# 的 1.5s USB 稳定延迟（application/safe_flash.rs:265-275）——用户以为对槽已换新实际刷了当前槽；找回延迟+降级时告警可见。
- **B25** SafeFlashPage/RootPage 占位与死按钮：「未连接 ADB 设备」硬编码未订阅快照、"当前分区: --"不展示 report_stage、回锁 BL 永久 disabled 死按钮（后端无命令两侧都没有）、RootPage 官方 KSU 固定文案"Vivo KSU APK 已校验"与实际状态无关。
- **B26**（⑪）租约 Ed25519 用 `verify`、pinset 用 `verify_strict`——同为服务器签名宽严不一（lease.rs:306-308 vs pinned_tls.rs:724-726）；不可伪造性无差异，建议 lease 也 verify_strict 统一口径。
- **B27**（⑪）退出码常量双真源：protection/vmp.rs:122 导出 PROTECTED_EXIT_CODE=70，tauri/exit_supervisor.rs:20 重抄不 import——改其一即静默漂移；修法 `use nwflash_protection::PROTECTED_EXIT_CODE`。
- **B28**（⑪）VersionClient 会话级永久缓存（version_client.rs:74-103）：session_result 一进程只查一次，服务端中途上调 min 版本（强制更新）本进程不可见（426 路径可兜底 force exit，版本页/提示不更新）；建议加 TTL 或 426 时失效。
- **B29**（⑪）dead export 清单（全仓无非测试调用方）——domain：RomInfo（与 infra RomResolveResponse 同形，二选一）、log.rs 整模块、app_page.rs 整模块、ProductIdentity/Page/NavigationCategory/IDENTIFICATION_PAGES/DEFAULT_API_VERSION（值 "0.1.0" 与 DEFAULT_APP_VERSION "1.0.1" 不一致）、PartitionTransferProgress、format_partition_size、TraceUploadAckV2 别名；protection：dispatch_protection_decision + ProtectionSelector/DecisionInput/ProtectionDecision/ProtectionFailure/encoded_selector/ProtectedAdmission（decision.rs 全部——与生产路径平行的完整决策 API，无人接线）、verify_signed_lease（自认 test-only，建议 cfg(test)）；application：operation_id_for_tests 无 cfg(test) 包裹进生产二进制。与 A30/C-19 合并处置。
- **B30**（⑪）UsageLogEntry(V1) 与 TraceRunV2(V2) 契约漂移（operation.rs:84-102）：event_id 同名异义（V1 本地 `{epoch_millis}-{seq}` vs V2 UUIDv7）、duration_ms i64/u64、status 自由串 vs 枚举——V1 是退休桥，迁移完成前列入监控即可。
- **B31**（⑪）resolve_rom 与 handle_api_error 对 404/5xx 的报文来源不同（api_client.rs:707-721 vs :995-1019）：分类一致仅 message 来源不同（服务端文案截 300 字符 vs 静态兜底）；注释已声明理由；风险是服务端可控文本经 rom_lookup_message 流向 UI——UI 直显路径未过 sanitize，建议补齐（关联 A39/C-15）。
- **B32**（⑫）重复启动投屏的错误文案被通用消息吞掉（mirror.rs:272-279）：runtime.start 具体错误（如「ADB 投屏已在运行。」）被换成 MIRROR_START_FAILED_MESSAGE（"内部错误…"）发前端；真实原因只进操作日志。修法：started_tx 透传 error.to_string()。
- **B33**（⑫）UAC 取消文案在命令层被替换（driver.rs 命令层 :60-62）：result_to_domain_error(Canceled) → "用户取消: 运行被用户取消"，覆盖 windows 层精心分类的「已取消管理员授权，未安装驱动。」（driver.rs:1074）；C# 保留原文案。修法：UserCancelled 分支 .to_string() 透传。
- **B34**（⑫）投屏重启上限差一（mirror.rs:301）：`>= MAX_CONSECUTIVE_MIRROR_RESTARTS` 达 3 即停 vs C# `> Max`（第 4 次才停），注释自称「对应 C#」——见澄清 C-49。
- **B35**（⑫）双监督循环竞态虚增失败计数（mirror.rs:640-666 测试直接同步 await；真实 mirror_start :396-405 与旧循环 1s 重启延迟并发时，旧循环 start 失败「已在运行」被 note_restart_failure 计入，加速 abandon）。修法：start 失败原因是「已在运行」则不计。
- **B36**（⑫）mirror_stop 等全屏障拖到无关共享操作结束（mirror.rs:412 wait_until_idle 要拿满许可+设备许可——与 A16 同根，此处记录投屏侧影响）。修法：只等待自己的 Mirroring 终结（per-operation join）。
- **B37**（⑫）驱动前置失败不写操作日志区（drivers.rs:26-27 驱动包缺失直接返回字符串，无 report_preflight_failure 对等物）——与 A40 家族（preflight 不进日志）同族。
- **B38**（⑫）退出期间投屏停止最多滞后 1s（mirror.rs:306 裸 sleep 后才查 cancellation/admission）——改 select 即可。
- **B39**（⑬）session_stop 非幂等（session.rs:35-39）：auth_logout_inner 容忍 NotStarted，session_stop_inner 直传「会话未启动」——前端顺序 await session_stop→auth_logout，双击/重试登出时第二次 session_stop 报错中止整链，auth_logout 永不执行，「登出失败请重试」死循环。修法：NotStarted 同样容忍（仍清 token/能力/generation，保证幂等收敛）。
- **B40**（⑬）VersionClient 进程级永久缓存（version_client.rs:74-103，session_result 只写不清）：启动断网 → SoftwarePage/RootPage 整个进程寿命显示「最新版本: 未知」，管理员上调 min_version 后 version_check 永不感知（426 心跳兜底仍在，仅展示与门禁语义陈旧）。C# 每次调用重新查询。修法：失败兜底不入缓存或加 TTL/登录代重置（与 A18 同项两源确认——A18 即本条，⑬ 独立复核并补 C# 对照）。
- **B41**（⑬）software_status/resource_inventory 同步命令阻塞 async worker（software.rs:19-30、resources.rs:189-202）：tauri 宏把 sync fn 包成 async 直接跑在 tokio worker 上（无 spawn_blocking），DriverStore 扫描/注册表/INF 递归/整 exe SHA-256 慢盘下阻塞数百 ms 至数秒，与心跳共用运行时可拖慢单次心跳周期。修法：改 async + spawn_blocking。
- **B42**（⑬）finalize_login_session 半失败残留 generation（auth.rs:110-118）：install_generation 成功后 publish_session 失败直接返回 Err——旧会话已停、新 generation 已装入监督器但无 token/能力/生命周期，失败路径不回滚 clear_generation，状态机「半发布」。修法：失败分支补 exit_supervisor.clear_generation。
- **B43**（⑬）try_acquire_idle 错误折叠（auth.rs:99-102,148-151、session.rs:31-34）：Disposed/ExitPending/Terminating/InProgress 四种语义一律「已有任务正在进行中」——退出流程中点登录/登出显示误导文案。修法：ExitPending/Terminating 分支改「应用正在安全退出」。
- **B44**（⑬）准入块判定三处复制粘贴（device_identity.rs:97-109、device.rs:493-505、root_ota.rs:213-225）：同一 match 三份拷贝，admission_reason_from_domain_error 靠比对中文串反查 reason——一处改语义另两处漏改即静默分歧。修法：收敛 device.rs 单一实现。
- **B45**（⑬）online_sessions 401 复用登录错误文案（online.rs:32 → api_client.rs:58-60）：会话中途 token 吊销时在线列表报「用户名或密码错误，或账号不可用」——误导排障。修法：get_online 失败映射独立文案（「登录已失效」类）。

---

## C. 逻辑混乱待澄清（按用户规则记录，不拍板）

> 完整 40 条编号矛盾见逻辑扫描报告（已存任务输出）；以下为高优先澄清项与汇总。扫描 agent 建议优先澄清：1、9、10、34、35（与 busy 缺陷同族或直接影响刷写安全语义）。

1. **「忙」的边界**：授权等待期算不算"任务执行中"？（现修复为"许可持有即忙"）
2. **投屏与心跳退出**：只挂投屏不看会话时，心跳连败退出杀投屏是否期望行为？
3. **同一物理操作三种超时制度**（windows-A）：C# fastboot flash = IO 无进展 600s；Rust quick_flash = 墙钟 FLASH 30min；Rust 线刷 = 无超时。
4. **退出码 0 是否等于成功**（windows-B / 逻辑扫描 #34/#40）：quick_flash「必须扫描输出」vs safe_flash「完全信任」。三处（run_required、run_partition_flash、fastboot_partition_exists）。
5. **两个 fastbootd 等待循环失败语义相反**（windows-C / #15）：safe_flash 瞬态失败致命化 vs quick_flash 容忍继续等；且 quick_flash:488 注释说「自动流程必须传入有界时长」但生产全部无截止。
6. **两个同名 `terminate_process_tree`**（windows-D）：process.rs vs mirror.rs 成功判据不同。
7. **fastbootd 等待「一台 unauthorized ADB + 一台 fastboot」报「多台设备」**（#6）：Unauthorized/Error 也算一台"已连接"参与并存判断——冻结文案与场景（授权弹窗未点）不符。
8. **会话有效性五个独立标志无单一仲裁**（#11）：running/healthy/session_id/generation/has_token 并列暴露，窗口内互相矛盾（running=false 但 token 仍在）。
9. **应用层 MirrorService/FileTransferService 状态机重复或死模块**（#12）：生产零调用，状态由 Tauri 层重实现；FileTransferService 全库零消费者且嵌恒败 API。
10. **忙的三套表示并存**（#10 / 扫描建议优先）：许可派生 is_busy()（心跳已接）、快照 kind 派导（前端广播/手动刷新）、admission_state 派生（mirror 停投屏）。授权期三者不一致。是否收敛为唯一定义？
11. **safe_flash 预检计划与执行计划双输入**（#13）：预检 current_slot=None 且无条件计 misc，执行现场重读——用户确认的任务数与实际刷写可多可少。
12. **手动 vs 自动设备刷新对"忙"假设相反**（#35）：手动只认 Flashing 阻塞，自动被任何通道（含投屏/哈希）阻塞——只读刷新比刷写更严，与共享通道设计初衷相悖。
13. **Goodbye 变体两层语义相反**（#5）：auth.rs 对发出的告别无条件返回 Goodbye；session_lifecycle 把它当「活动心跳丢租约」终局退出（该分支恒 active=true 结构上不可达，见死分支）。
14. **401 双政策**（#38）：操作授权 401 立即拒绝，心跳 401 静默计数 50s 后才退——会话吊销中间态两个消费方假设相反。
15. **ROM 专属错误文案挂通用路径**（#7）：404/402 的「未找到对应版本的 ROM」被 login/heartbeat/online 等全部端点共用。
16. **「需要更新」双文案双产品名**（#8）：VivoKsu（api_client 硬编码）vs 奶蛙Flash（lib.rs 426 分支），服务端 message 被丢弃。
17. **数据目录三名并存**：`%LOCALAPPDATA%\Nwflash`、`%LOCALAPPDATA%\VivoKsu`、`C:\nwflash`——settings.json 与 operations.log 分属两个产品名目录。
18. **10 连败退出复用 ServerForced**（#16）：本地兜底被归类"服务端强制"；HeartbeatUnavailable 枚举零构造。
19. **死代码/死参数清单**（#18-#30、#22）：HeartbeatUnavailable、Goodbye 分支、SkippedBusy、AdmissionCheckedExecutor 恒 Idle、`_operation_task`、带坑的 `SessionLifecycle::new` 构造（无忙判定→照常计数退出，违背自身文档）、prepare_preset_image 整命令、QuickFlashOptions.target、OtaDownloadPlan.memory_cap_bytes、DEFAULT_API_VERSION、trace V2 三重状态（宣称封锁/pub 导出/零使用）、SafeFlashStagingOutcome 生产退化、`parse_fastboot_rs_output` 名不符实、misc 计入 flashed_partition_count。
20. **`fastboot_output_reports_failure` 用在 ADB 输出**：有意纵深防御还是作用域外溢？
21. **双槽 gap 内 `--` 占位设备快照**：预期 UI 表现？
22. **trace 毫秒 vs usage/日志秒双纪元**（#32）：字段名不带单位。
23. **components.css 约 64 行超出 handoff 记载**：提交时如何对账？
24. **`-T sh -c` 注释与实现对撞**（ROOT 审计 A）：root.rs:1022 注释宣称该形态实测必失败，同文件 vendor_boot 链大量使用——两处必有一处错。「实测语法错误退出 1」是在哪个 adb 版本、哪条脚本得出的？vendor_boot 修补链真机验证过吗？
25. **换设备语义两套相反答案**（ROOT 审计 B）：quick_flash 判"预检设备≠执行设备"为致命（注释：跨设备变砖事故）；safe_flash 测试注释「must not reject a changed preflight serial」。线刷对一致性要求理应不低于快刷——"不拒绝"是刻意产品决策还是移植未加分辨？
26. **ROOT 模块内三种序列号绑定语义并存**（ROOT 审计 D）：全自动每阶段重读 / 手动修补刷写全程强绑定 / 线刷执行时重绑——C# 只有一种（会话 serial 贯穿）。
27. **install_root_manager 的 is_current 后置校验是孤例**（ROOT 审计 E）：其余 root 命令全靠 scope.commit 原子拒绝——两种防陈旧发布风格并存。
28. **"仅分区刷写触发回调"与 misc 的归类矛盾**（ROOT 审计 C）：文档说仅 fastboot flash 分区命令触发回调，但 wipe-data 也是一条 fastboot flash 命令、失败即中止不弹窗——"分区刷写命令"在文档语言与元组语义中是两个集合。
29. **root_ota_check 是否有意在 adb+网络全程持全套 idle 租约**（对抗审查）：设计意图若是「检测期间不允许任何操作」则 A23 修法落在 A3 超时上；若无心，租约范围本身值得收窄。
30. **busy 是否需要最长持续时间看门狗**（对抗审查）：许可派生后「忙」在无超时命令挂死时无限期——是否需要 N 分钟无操作体推进则视作空闲的看门狗？设计决策。
31. **服务器 426 强制更新的会合点**（壳层 L1）：后端自动退场（在途操作跑完即 terminate(70)，弹窗最多活「在途操作时长+1s」）vs 前端 UpdateRequiredDialog 假设用户可交互（带 onQuit 按钮）——C# 真源是「弹窗等用户自己点退出，进程继续等」。你要哪种？
32. **Delayed-ServerForced 优雅通道被同决策的即杀回调短路，监督器该档路径生产不可达**（壳层 L2）：terminal_force_exit 依次调 on_terminal（监督器 delayed：等空闲+goodbye+退 70）和 on_force_exit（lib.rs:947 terminate(0) 即杀）——后者同步必先到，监督器 delayed-ServerForced「在途操作自然跑完再退」语义生产永不生效（测试专门保活这条死路径）。ServerForced 要不要保留 Delayed 通道？不要的话删掉死路径（连带测试语义重述）。
33. **磁盘 IO 错误塞进 Transport 变体**（网络 L-2）：remote_firmware 把建目录/写文件/删除失败全归「Transport」，上层再折叠「无法读取服务器固件」——磁盘满与网络故障不可区分。
34. **409/410 的客户端预期行为哪边是契约真源**（网络 L-6）：服务端 index.ts:700-704 注释与 session_lifecycle 实际分类不一致（见 A36）。
35. **UploadError 二分建好未消费**（网络 L-1）：tauri 层有 Permanent/Transient 精细映射，infrastructure 层 flush 对两者行为完全相同——若未来补全死信语义，429 误分类立即变丢数据缺陷（见 A38）。
36. **动态 pinset 与 trace-v2 是两套「建好未接线」体系**（网络 L-3/L-4）：5700+ 行 trace 协议栈全 allow(dead_code)；pinset 缓存/bootstrap/版本地板全套已建但生产零调用——启用前先排 A35 启动地雷。接线计划还在吗？
37. **drivers.rs exit_code 死字段**（下载 L-1）：`exit_code_for_operation` 仅 exit_code==0 时写 Some，非零早退 Err——`DriverReinstallDto.exit_code` 恒为 0；三份 Arc<Mutex<Option<i32>>> 克隆传递只为落一个永远不变的 0。
38. **firmware.rs update_partition_stats 启发式**（下载 L-2）：`seen.len()>1` 时 successful+1 的"切分区=上一分区成功"推断；terminal 强制 `completed = total.max(seen.len())` 且 `successful = completed`；failed/skipped 字段从未写入（恒 0 死字段）。
39. **线刷 artifact 双重 replace 舞蹈**（下载 L-3）：先 replace（非 owned）入库再 get+replace_owned 转 owned——两次入库只为改标志；lock 中毒分支留下指向已删目录的非 owned 条目。
40. **resolve_manager 兜底返回未校验的 cached 路径**（下载 L-4）：两个候选都校验失败时 `unwrap_or(cached)`——下游会再验触发下载能自洽，但单看返回值无法判断可信性。
41. **上传/下载/事务链 vs 非事务链的进度语义**（下载报告边界）：事务路径有 FILE_TRANSFER/APK 超时与进度转发，非事务 list/delete 无超时无日志（A40）——"事务"是隐式分类，无类型区分。
42. **OperationAuthorization 同名双类型**（⑪）：infrastructure/api_client.rs:491（wire DTO，snake_case）与 application/operation_coordinator.rs:395（gate 结果，公开字段）——tauri lib.rs:746 手工互转；同层同名单类型不同 serde 形态。
43. **`with_running_dispatch` 同步派发闭环全仓无生产消费者**（⑪）：调用全在 tests；tauri 命令用 spawn_blocking + run_command_with_cancel 不经此门。整套 RunningDispatchScope/重入检测是已测试未接线能力（同 trace_spool 的 Wave2 seam 定位）——若是路线图则无碍，若是被遗忘的收尾则「准入模型宣称的最终副作用原子性」未落地。
44. **心跳 tick 恒传 active=true，Goodbye/ApiError 分支在 terminal_classification 中不可达**（⑪ P2-13）：session_lifecycle.rs:511-515（Goodbye 仅 !active 时产生）、:655-660（401/403/409 分支无调用路径）——与 #13（Goodbye 变体两层语义相反）同族，分类表是行为真源的历史残留；删除或在注释同步说明。
45. **时钟容差常量三层三值**（⑪）：protection MAX_CLOCK_SKEW_SECONDS=60（租约未来容差）、tauri MAX_WALL_CLOCK_REGRESSION_SECONDS=120（回退锚点）、session_lifecycle 各 timeout——名字与层级不统一，语义各自成立但无一处汇总说明。
46. **`DomainError::RemoteApi` 生产构造点疑似缺失**（⑪）：全仓 grep 仅 coordinator 测试构造（operation_coordinator.rs:1332）；生产错误→DomainError 映射实际都走 AuthorizationDenied/InvalidOperation/Internal——变体接近死代码但保留在公共枚举。
47. **quick_flash 命令层「公开文案」与协调器同名函数语义互斥**（⑪ P2-1，见 A53）：三个同名职责函数，一个全脱敏、一个透传，注释声明与实现相反。
48. **V1/V2 event_id 同名异义**（⑪ P2-9，见 B30）：记录在案，迁移时统一命名。
49. **投屏重启上限差一**（⑫ L1，见 B34）：常量注释「对应 C# MirrorService.MaxConsecutiveRestartFailures / 超过上限」（mirror.rs:24-26），代码 `>= 3`（达到即停）而 C# `> 3`（超过才停）——连续第 3 次失败到底该不该停自动恢复？
50. **scrcpy 缺失文案的死测试**（⑫ L2，mirror.rs:537-544）：测试声明「校验错误文案中的安装指引仍保留」，但只对本地字面量做 contains 断言——生产代码已改为 failover 自动下载，不再产生该文案。删测试还是改写为「缺失时走下载器」？
51. **驱动 DTO exit_code 字段去留**（⑫ L3，drivers.rs:14-20, 65-68）：字段承诺诊断信息但成功路径恒为 0（非零已转 Err，unwrap_or(0) 分支不可达）——保留给未来非零诊断还是删除？（与 C-37 下载报告的 drivers.rs exit_code 死字段观察同一条。）
52. **投屏期间设备刷新该不该停**（⑫）：C# 镜像不置 busy、投屏期间刷新照常；Rust 投屏 busy → 自动刷新全停 + 手动刷新不拒（两侧语义不同但方向一致的部分是手动）。修 A57 方案 A 后两侧都恢复 C# 语义——确认即可，无需单独拍板。
53. **投屏测试缺口清单**（⑫）：自动重启循环零覆盖（P1-2/P0-2 计数与闸门语义）、reconcile_after_device_update 拉起、投屏期间刷新被跳过、stale_pids 强杀、busy→登出 InProgress、mirror_stop 与无关共享操作并发、drivers 命令层仅测 ini 拼接——共 7 处。批准 A57/A58/A60 修复时连同补测试。
54. **VOTA 固件目录以 bbk 代号还是 ro.product.device 为键**（⑬ 混乱-1）：Rust device_identity.rs:67 对 safe_flash 与 root_ota 统一返回 ro.product.device；C# 云提取（RootViewModel.cs:402）优先 bbk 代号（DPD2221B）、回退才是 ro.product.device（PD2417），C# SafeFlash 用后者。服务端把 pd 原样透传 VOTA——若 VOTA 目录以 bbk 建键则 Rust ROOT 检测系统性 404，反之 C# 404，两者不可能同时为真（除非双收录）。**需用户拍板：Rust ROOT 检测要不要跟 C# 一样优先 bbk 代号？**
55. **版本回退候选顺序真源**（⑬ 混乱-2）：Rust device_identity.rs:68-73 回退序 display.id → incremental → vivo.os.display.id（与 C# SafeFlashViewModel 一致），但被替换的 C# RootOtaCloudExtractService 用 VivoVersionParser（incremental 优先）。display.id 与 incremental 均非 generic 且不同值时两条链可能命中不同固件。边角：bbk 尾段 rsplit('_') 遇尾下划线 Rust 得空串跳过、C# 取 "B"。**需用户拍板：云提取版本回退序以 VivoVersionParser 还是 SafeFlashViewModel 为准？**

---

## D. 已落地并验证（本轮修复，未提交）

1. **is_busy 改许可派生**（operation_coordinator.rs）：删 AtomicBool，`is_busy() = device_lock 可用许可==0 || host_lane 可用许可<上限`。一次修掉 P0-1 授权期假空闲、P0-2 共享并发假空闲、P1-1 abort 卡 true、P1-5 preflight 竞态窗口。
2. **终态快照 started_at 统一写真实起点**（Completed 原 None、Canceled/Failed 原为结束时刻）。
3. **新增 4 回归测试**：授权中即忙、共享并发不清零、abort 复位、终态起点时间。
4. **session_lifecycle 两个时序炸弹测试修复**（兜底租约固定序号第二次出现触发 SequenceRollback——改单调递增）。
5. **lib.rs 心跳忙判定接线**（前次会话）：从恒真 admission_state 改 `coordinator.is_busy()`。
6. **lib.rs:665-668 忙判定注释同步**（逻辑扫描 #1）：删除修复前旧语义描述，改为许可派生语义——防止后续维护者被误导。
7. **report_preflight_failure TOCTOU 闭合**（对抗审查 P2）：`is_busy()` 检查改为 `try_acquire_idle` 原子守卫——获取租约即证明空闲，租约在手期间任何 run 被拒，check→write 间不可能插入新操作。
8. **device.rs:811-814 陈旧注释修正**（对抗审查 P3）：原注释宣称发现链持 idle 租约，实际不取许可——新语义下误导放大，已改为如实描述。
9. **terminal 测试断言加强 + Canceled 用例**（对抗审查 P3）：started_at 加时间戳区间比较（防回归为结束时刻）；新增 Canceled 分支独立测试。lib 61→62。
10. **HOST_LANE_PERMITS 同源锚定注释**（对抗审查 P3）：is_busy 文档注明判定与构造共用常量、未来按配置构造时必须同步。
11. **A1 修复（P0 伤设备级）**：command_timeout::for_command 补 `push`→TRANSFER、`shell+su` 且引号串含 `dd `/`blkdiscard`/词边界 contains →FLASH。ADB Root 大分区 push/dd 不再被 CONTROL 60s 中途强杀（半写 super 不可开机）。新 4 测试用例。
12. **A2+A6+A7+A17 修复（线刷安全族，safe_flash.rs）**：run_required/run_partition_flash/fastboot_partition_exists 全部接线 run_with_timeout（flash 30min 兜底、其余 60s）；退出码 0 时扫描 FAILED/remote error 协议失败行（quick_flash 语义下沉到线刷，半刷不再判成功继续下一分区）；wait_for_fastbootd 瞬态探测/getvar 失败改「本轮作废继续等」（对齐同仓 quick_flash 循环），墙钟截止从第二次尝试起生效（防 60s 探测×360 次放大成 6 小时窗口）；窗口文案按 attempts×interval 真实换算（180s 不再误报 360s）。
13. **A10 修复（usage 明细串台）**：operation_details 单 Vec+clear() 改 BTreeMap<operation_id, Vec> 分桶；终态只取走自己的桶；桶 500 条/总 16 桶上限。
14. **A11 修复（暂存清理失败毒化成功）**：quick_flash ADB Root 清理失败降级 report_warning，刷写成功不再被改判失败。
15. **A12 修复（preflight 失败不进日志，quick_flash 侧）**：批次请求校验、固件确认、双槽预检（命令失败与计划构建失败两分支）统一接 report_preflight_failure。
16. **A13 修复（ImmediateTamper 无界等待）**：篡改退出等待在途操作以 750ms 为上限（tokio::time::timeout），到点放弃租约直接进入有界收尾；Delayed 保持自然等待。
17. **A27 修复（退出码语义倒置）**：Delayed 优雅收尾传 0，仅 ImmediateTamper/worker panic/通道消失传 70；call_terminator_once 参数化退出码。
18. **A60 修复（投屏重启计数三处复位缺）**：begin_manual_start/stop/set_auto_enabled(true) 全部复位 consecutive_restart_failures（对齐 C# StartAsync/StopAsync/ClearDeliberateStop）。
19. **A61 修复（failover 下载不可取消）**：start_plan 供给从预构造 future 改为闭包工厂，门内以操作体真实 CancellationToken 调用；ensure_installed 下载可被 operation_cancel 终止，不再与 busy 恒真叠加成永久锁死。
20. **B32 修复（启动失败文案吞掉）**：供给失败与 runtime.start 失败的具体原因直接透传前端（删 MIRROR_START_FAILED_MESSAGE 通用文案）。
21. **B35 修复（双循环竞态虚增计数）**：重启循环内 start 失败若为「已在运行」不计失败。
22. **B34 连带（infrastructure api_contract 预存在失败修复）**：resolve_async_maps_insufficient_credits_status 断言停在「402 固定中文文案」旧语义，而 HEAD 上 resolve_rom 已改为优先透传服务端 error 文案（注释声明有意设计，对应审计 B31 两路径并存的记录项）——测试改为验证 body 无 error 字段时的 402 兜底映射。该失败自仓库重建提交（1f552c6）起即存在，与修复无关（A/B stash 验证）。
23. **线刷/固件提取确认弹窗生命周期死锁修复（用户 09-14 报障：弹两次且第二次关不掉）**：SafeFlashPage/FirmwareExtractPage 确认弹窗此前在整个执行期停留且 `onClose` 执行中置 undefined、按钮全禁——分区失败决策弹窗叠在它上面（用户所见「弹两次」），执行失败后 catch 分支又从不清理弹窗状态，取消命令被后端拒绝（会话失效/预检失效/执行中）时三条关闭路径全死，弹窗永久卡死。修复对齐 e09aec6 已拍板的快速刷写模式：确认即关窗（执行状态由进度面板与「停止操作」接管）、执行失败恢复弹窗（后端 staging 在盘可直接重试）、取消改乐观收起（后端拒绝仅作错误提示）+同步防连点护栏（ref）。随行修正 e2e 两处预存在漂移断言（`安全线刷确认`/`仅刷写可直接镜像分区`/`在线 OTA` 均为 e09aec6 改造后即失效的旧文案，实际运行必然失败）。新增 5 回归测试（确认即关窗×2、执行失败恢复×2、取消被拒仍可关×1）。LineFlashPage 检查无同族缺陷（执行期间弹窗按钮不锁、关闭始终可用），未改动。

验证基线（2026-09-14 更新）：**workspace 51 套件全绿**（application 17 套件含 lib 62 + 集成全过、tauri 327 含新 A1 用例、infrastructure api_contract 23/23、windows/domain 全过；CARGO_INCREMENTAL=0，rustc 1.98.1 增量编译器 ICE 已用清缓存规避）；check 通过；前端 tsc 0 错误、test:ui 214/214（含弹窗死锁修复新增 5 用例）。

## E. 审计进行中（已派 agent）

（无——13 份报告全部收齐，2026-09-13。）

## L. auth/session/online/software/version/release_probe 审计已核实无问题项（摘要，防重复审计）

凭证内存驻留（password Zeroizing 早于任何 await、token 全链 SecretToken、DTO/Debug 无 token、服务端错误回声不穿透 WebView，测试覆盖）；login lease 校验（签名+九项绑定在保护圈叶子内，未签名响应零发布）；并发登录串行化（finalize 全程持 idle 租约，无双发布窗口）；logout/closeout 顺序（能力失效先于 token 清零、generation 只清当前代、持租约时长与对抗审查确认的有界结论一致）；session_capabilities（epoch 单调、失效-发布互斥、epoch 耗尽 fail-closed、预置能力登出统一失效清理）；online.rs（token 仅 request_scope、DTO 无 username/IP、宽松反序列化对齐 WPF、30s 超时）；version.rs（公共端点/current 参数/缺省容忍/ALLOW_ALL 放行对齐 C#）；software.rs 与 release_probe.rs 非双真源（运行时就绪度 vs 发布契约自检，角色不重叠）；DEFAULT_APP_VERSION 双写有测试钉住；release_probe 严格单参数校验 fail-closed + 手写 JSON 转义正确；lib.rs 命令注册完备且仅一次、心跳回调序号守卫、忙判定按许可派生接线（已修项验证通过）；session_start 拒绝设计（前端零调用点，防绕过登录）；device_identity 准入原子复查 + 错误固定文案不泄 serial；auth_validate_token 网络异常按未登录处理（对齐 C#）。

## K. mirror/drivers 审计已核实无问题项（摘要，防重复审计）

build_start_plan（serial 取自已确认快照、ADB 环境变量传递、--stay-awake 与 C# 一致，测试锁定）；MirrorStatusDto snake_case 契约 + 前端双命名容错；start_plan started 信号握手（供给失败/spawn 失败/run_shared_async 失败各路径发信号或断链兜底，spawn 失败集成测试验证终态与许可释放）；stale PID 策略逻辑自洽（stop 终止失败记录 PID、下次 start 前强杀，防双实例）；共享通道方向正确（投屏不拦截设备独占操作，有测试）；手动设备刷新在 Mirroring 期间不被拒；bind_device_monitor reconcile 仅在身份变化时触发（对齐 C# forceFire:false 心跳语义，避免 3s 重启风暴）；**驱动链完整性工程远超 C# 原型**（镜像快照→编译期 SHA256→冻结解压→revalidate→提权窗口期句柄守卫拒绝写/删/换，注入/reparse/WINDIR 伪造均有对抗测试）；单条 pnputil 单次 UAC、绝对路径无通配符（较 C# 收紧）；write_vivo_adb_usb_ids 幂等（exit 0 才写、去重，测试）；驱动 staging 各路径（含取消）均清理（测试验证）；UAC 拒绝 1223→UserCancelled 分类在 windows 层保留正确；前端启动驱动提醒只要求 ADB+Fastboot（对齐 C# CheckAndRemindDriverAsync 语义）。

## J. domain/跨 crate 审计已核实无问题项（摘要，防重复审计）

crate 依赖方向（domain←windows/protection←infrastructure←application←tauri，无循环无上层泄漏）；TokenDigest 恒时比较（ct_eq 仅秘密字段）；租约时间窗校验（expires<=issued 拒、issued>now+60 拒、expires<=now 拒、Login seq==1/Heartbeat 严格递增——重放/回滚 fail-closed）；SignedEnvelope 校验链（签名覆盖 base64url 原文、Zeroizing 全程、VerifiedLease 私字段不可伪造）；admit_local_operation + SessionCapabilityScope（epoch 化能力租约、原子序列检查、ClockRegressionAnchor 120s 回退锚补「断网拨钟续命」缺口）；保护圈分类 fail-closed（未知 wire index→true、穷举 match 编译期锁、高危表收在 VMProtect 叶内）；VMP 探针不可用语义（Release ProbeUnavailable→完整性退出，IntegritySignals::unavailable 不伪装阴性）；trace_v2 wire 契约（deny_unknown_fields、canonical UUIDv7 硬检、safe-integer、serde 错误不回显、Serialize 故意不实现防越权序列化）；trace 凭据脱敏管线（逐字节状态机、HighRisk fail-closed、跨 chunk 拼接不可行、sentinel receipt 绑死 leaf、6.5MB 饱和拒绝）；pinned TLS 生产配置（双 pin 备份、no_proxy、重定向三重断言、版本回滚双闸+高水位、原子缓存替换失败保留旧 envelope）；心跳生命周期（忙时不计数且清零、能力 capture/refresh_verified 原子换 lease——与 C# 注释一致，**is_busy 修复接线确认**）；服务器授权门 fail-open 为文档化决策（Transport/5xx/超时/403/409→allow、401/426/Integrity→deny、封禁由心跳 force_exit 兜底、5s 黑洞不占门）；分区表解析（空表拒绝、槽位/高风险标记、hex/dec 双解析、字节级 banner 前缀比较防 UTF-8 边界 panic）；quick_flash 域计划构建（双槽 has_slot 强校验、不静默少刷、sparse 魔数/空文件/超容量三重拒绝）；下载/Range 一致性（Content-Range 三元组严格校验、take 防超读、常时比较 constant_time_eq）；crash/integrity 上报契约（字符集+长度白名单、16KiB/32KiB 上限）；AdmissionGate 毒锁恢复（poison→强制 Terminating+清空派发凭据 fail-closed）。

## I. 下载资源审计已核实无问题项（摘要，防重复审计）

上传/下载事务链（临时文件独占+sync、no-replace promote、取消/失败清理、清理失败绝不伪装成功、测试矩阵完整）；shell 注入面（quote+validate 双层）；scrcpy 供给链（zip slip、固定 URL/摘要/大小三元组、manifest 逐文件 SHA-256、失败不删已批准包、stale 清扫）；payload_dumper 供给链（Windows 安全路径白名单、仅 exe 入缓存目录消除 DLL 劫持、编译期固定摘要、损坏缓存双检重下）；资源 URL 全部编译期常量（无服务端下发，等效白名单，审计问题 1 的回答）；远程固件 Range 读取（206/Content-Range 边界校验、常时比较）；快照/工件运行时 TOCTOU（提取产物复制私有 staging、旧 result_id 失效、执行计划校验路径必在 staging 内、capability UUID——审计问题 2 的回答；快照仅大小校验无内容哈希如实记录边界）；payload 提取（120s 判死、三段发布、回滚、expected-size 硬校验、分区名白名单）；驱动链（哈希编译进二进制、快照+解冻+双重复验、单次 UAC、1223=用户取消，审计问题 4 的回答）；operation_log 写盘/崩溃恢复/环形上限/与 usage 分界（审计问题 5 的回答）；协调器接线（10 入口 run_shared、files 5 入口 run_async，审计问题 6 的回答）。

## H. 网络层审计已核实无问题项（摘要，防重复审计）

证书钉扎链完整（WebPKI 先行+SPKI pin 叶子或中间、NoKeyLog、redirect(none)+二次校验、pinset 信封 ed25519+版本地板+高水位防回滚+原子替换）；生产/调试校验差异正确（production() 不受 debug 开关影响、release 缺 build_id 拒启、protected feature 不改变网络校验、e2e 门禁三重约束）；秘密处理（Zeroizing+Debug 脱敏+Bearer set_sensitive+退出 zeroize+崩溃上传价值过滤+owner SHA-256）；幂等键（usage event_key、crash event_id 跨启动稳定、trace 收据 compile_fail 强制、心跳 sequence CAS）；trace 协议分类 fail-closed（若启用）；OTA 分段下载（Content-Range 逐字节校验+暂存原子提交）；RemoteAssetDownloader（4 候选 failover+双限+事务化提交+SHA-256）；zip-slip 双解压器防护+安装后 manifest 复检；超时叠加方向（所有外层<内层组合安全）。

## G. app 壳层审计已核实无问题项（摘要，防重复审计）

invoke_handler 完备（60+ 命令全注册）；AUTHORIZE_TIMEOUT=5s 对齐 C#、授权可取消、黑洞请求放行不占门；本地保护门 fail-closed 分类与启动时序；退出监督器状态机（ExitPending 单调迁移、750ms 截止不重启、陈旧 generation 线性化 32 轮并发测试）；退出清理顺序（capability 失效先于 token zeroize、mirror 停止在清理首位）；心跳语义逐条对齐 C#（5s/10s/3s/10 连败/忙时清零）；两通道协调器互不阻塞；文件事务四档超时+no-replace promote+失败清暂存；子进程终止联动（taskkill+kill+2s reap+管子排空）；关窗空闲路径全链正常退出码 0；usage 持久化崩溃不丢；main.rs 崩溃钩子链式保留；protected 编译差异 fail-closed；分区写/备份跨设备防线完整。

## F. windows 审计已核实无问题项（摘要，防重复审计）

管子读线程并发排空/8MB 截断继续排空/观察器有界队列与慢观察者不阻塞/Debug 脱敏/spawn 边界完整性校验/驱动安装 TOCTOU 加固链/UAC 拒绝映射 UserCancelled/validate_device_path 防逃逸/设备行解析健壮/fastbootd 身份门+序列号绑定/刷新闸去抖/promote_without_replace 原子晋升/备份完整性校验/提取 120s 判死/shell_quote 等价 C#/exec-out dd 同构 C#/驱动检测三信号/拒绝环境注入/spawn 站点 allowlist 元测试——均无缺陷。
