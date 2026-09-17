# 三线安全审计报告（2026-09-09）

审计基线：HEAD `19ae972`（codex/vmp-release-completion），工作树除本报告外无改动。全程只读：未修改源码、未构建、未跑测试、未部署、未连接设备。

三线并行审计，每条发现均有 file:line 证据。已知已修项不重复报告，仅验证修复周边残余：
- 19ae972（ADB/Fastboot 命令层：ROOT_PATCH 5min 预算、预检超时、日志留痕）——修复验证通过，无残余
- 2f1edbb（固件 sha256 承诺 + 全局 Tauri 绑定）——ROOT 云提取链接线正确，但存在两条 P1 旁路（见 INF-1/INF-2）
- VMP 十轮加固（2026-09-08）——不在本次范围，未复核
- F-01（741ef97+819cf25）、F-02（59fe97d）、版本门（e17bd81）——验证通过，仅残留 P3 死代码

总计：**54 条 —— P0×0，P1×9，P2×19，P3×26。**

---

## 一、P1 全量清单（9 条）

### 服务端（cloudflare，5 条）

| # | 位置 | 问题 |
|---|---|---|
| CF-1 | `cloudflare/src/index.ts:960-987` | `/api/login` 无防爆破限流；用户不存在时跳过 PBKDF2 快速 401，时序可枚举用户名。schema 中 `login_attempts` 表未被 API worker 使用（user 门户有 `LOGIN_MAX_ATTEMPTS=8`，API 侧裸奔） |
| CF-2 | `cloudflare/web/src/index.ts:279-302` | 管理后台 `/api/login` 同样零限流、零锁定、无 dummy PBKDF2——全库最高权限入口可无限速在线爆破 + 管理员用户名枚举 |
| CF-3 | `cloudflare/web/src/index.ts:512-520` | 管理员重置用户密码不吊销该用户 API token、不清理会话。被盗 token 在"重置密码"应急动作后原样存活，可继续调 `/api/rom`、heartbeat、trace 上传。user portal 自助改密有 revoked-marker 协议，管理端未复用——语义自相矛盾 |
| CF-4 | `cloudflare/web/src/index.ts:344-347` | 管理员改密不要求当前密码。会话 cookie 被盗即可永久接管账号并踢掉真管理员 |
| CF-5 | `cloudflare/src/trace-v2-contract.ts:2-8` + ingest 全文 + `src/index.ts:805` | 认证用户可无限膨胀 D1：run 数量无上限、累计 chunk 字节无配额、trace 端点无请求频率限制、`/api/usage/logs` 连请求体大小限制都没有（对照 trace 有 1 MiB 流式限长）。数千请求打满库 → 全服务写入拒绝 |

### 桌面端（Rust，4 条）

| # | 位置 | 问题 |
|---|---|---|
| INF-1 | `nwflash-application/src/safe_flash.rs:1334-1346` + `nwflash-tauri/src/commands/safe_flash.rs:684-723` | **线刷在线整包下载链（5-9GB OTA）零完整性校验**。`resolve_rom` 返回的 `sha256`/`sizeBytes` 被整体丢弃（`url: rom.url`），下载内容直接进 payload_dumper 解包刷写。2f1edbb 自述威胁模型（明文 HTTP OTA + MITM 换包）在此链原样存活：恶意 boot/lk 分区 → 刷砖或持久化植入 |
| INF-2 | `nwflash-application/src/root_ota.rs:94-106` | root_ota 完整性门 **double-fetch TOCTOU**：verify（第一次全量下载）与 probe/extract（第二、三次 Range 请求）是独立 HTTP 会话，MITM 在校验通过后的响应间切换内容即绕门。`validate_range_response` 只验 Content-Range 格式不验内容 |
| WIN-1 | `nwflash-application/src/safe_flash.rs:482-484,513-515,625-627` + `nwflash-windows/src/process.rs:646-651` + `nwflash-tauri/src/commands/root.rs:2219-2233` | **SafeFlashExecutionService 整条线刷链无墙钟超时**。全部命令走 `executor.run()` 无超时默认实现；19ae972 建立的"任何命令不允许无限挂起"不变量在最危险路径上落空。USB 半断开/驱动卡死 → 进程永不退出，Flashing 准入门无限持有，不能继续也不能取消重试。`root_run_automatic` FlashFastbootd 阶段同样继承 |
| WIN-2 | `nwflash-tauri/src/command_timeout.rs:47-52` + `commands/quick_flash.rs:108-116` + `nwflash-windows/src/device_transport.rs:102-130` | **ADB Root 分区写落入 60s CONTROL 档**。`for_command` 用字面参数 `"flash"|"dd"` 匹配 FLASH 档，但 ADB Root 写路径的 `dd` 被包进 `su -c '...'` 引号脚本内部，不是独立 argv 元素——永不命中。数 GB super/system 分区 push/dd 超 60s 被中途强杀 → **半写分区**，boot 链分区即潜在变砖。日常操作路径，非极端场景 |

---

## 二、P2 全量清单（19 条）

### 固件/传输完整性（5 条）

| # | 位置 | 问题 |
|---|---|---|
| INF-3 | `nwflash-tauri/src/commands/firmware.rs:1335-1363` + `remote_firmware.rs:91` | 手动 URL 固件检测/提取链无完整性锚点 + `validate_http_url` 放行明文 `http://`，zip CRC32 是唯一"校验"。2f1edbb 注释自认有意无门（**待确认设计决策**） |
| INF-4 | `nwflash-infrastructure/src/ota_download.rs:897-904,698` | `validate_url` 只查非空不查 scheme；`Client::new()` 未设 `no_proxy()`/`https_only()`，系统代理可全量劫持在线 OTA 下载（与 INF-1 叠加成完整 MITM 链） |
| INF-5 | `nwflash-infrastructure/src/remote_firmware.rs:72-77,91` | 远程固件默认 client 同样无传输安全约束；Range 读取走系统代理；明文放行（与 INF-2 叠加） |
| INF-6 | `nwflash-infrastructure/src/vivo_firmware.rs:141-144` | tar 头分区大小 `sum::<u64>()` 可溢出：两个极值条目即溢出，debug panic、release 环绕后进度条错乱 |
| INF-7 | `nwflash-infrastructure/src/vivo_firmware.rs:316-331` | `parse_octal` GNU base-256 分支对 12 字节 size 字段做 12 次 `value << 8`，必然超 u64：debug 构建对恶意 tar 直接 panic |

### 命令超时体系（3 条，与 WIN-1/2 同根）

| # | 位置 | 问题 |
|---|---|---|
| WIN-3 | `nwflash-tauri/src/commands/partitions.rs:637-641` | 分区备份 dd 回读 `run_command_with_file_stdout_and_cancel` 超时传 `None`；不经 `for_command`，连字面匹配兜底都没有。大分区（super 10GB+）备份 USB 卡死 → 准入门无限持有 |
| WIN-4 | `nwflash-tauri/src/commands/files.rs:567,689` | `files_list` 与 `execute_file_command`/`files_delete` 超时 `None`，与模块头"任何命令不允许无限挂起"声明矛盾。adb devices/ls 卡死（驱动半加载常见）→ Discovering/Transferring 门无限持有 |
| WIN-5 | `nwflash-tauri/src/commands/device_identity.rs:16-28` + `commands/safe_flash.rs:678` | 设备身份 getprop 用无超时无取消的 `run_command`，在 Flashing 准入门内被调用——ADB 半连接设备即让准备阶段永久卡死 |

### 服务端资源/留存/脱敏（11 条）

| # | 位置 | 问题 |
|---|---|---|
| CF-6 | `trace-v2-redaction.ts:199-233` | 凭据跨"请求"分块可绕过服务端脱敏：契约允许 child-only 分批补块，凭据恰跨两个请求的块边界时两块独立匹配均失败 → 明文凭据入库，30 天管理端全文可见 |
| CF-7 | `trace-v2-contract.ts:169` + `crash-diagnostics.ts:126` + `trace-v2-retention.ts:27` | 时间戳无未来上界：`started_at_ms=MAX_SAFE_INTEGER` 的 run / 未来 `occurred_at` 的崩溃报告**永不清理**，且 `ORDER BY ... DESC` 让恶意记录永久占据管理列表顶部 |
| CF-8 | `src/index.ts:215-222,457-464` | `integrity_events` 完全无 DELETE；`admin_sessions` 过期行、usage_logs(v1)、access_logs、admin_audit_log 均无清理逻辑 |
| CF-9 | `trace-v2-redaction.ts:25-27,122-148` | 凭据正则多项式 ReDoS：`CREDENTIAL_KEY` 前缀与 `COMPLETE_PRIVATE_KEY` 在"匹配失败前逐位置重试"输入下 O(n²)，单请求 1 MiB 可含多个 16 KB 字段叠加到秒级 CPU |
| CF-10 | `trace-v2-redaction.ts` 全文 | IMEI/手机号/IPv4/序列号/本地路径（`C:\Users\<用户名>`）完全不在服务端脱敏范围；刷机输出天然密集包含 IMEI，管理审计页 30 天内全文可见 |
| CF-11 | `src/index.ts:805,824-854` | `/api/usage/logs` 无请求体大小限制（`request.json()` 无界读）；details_json 每条可存 500×16 KB 文本，与 CF-5 叠加放大 |
| CF-12 | `cloudflare/backups/predeploy_20260902_145529/api_users.json` | **明文落盘真实线上 token、PBKDF2 哈希与盐**。未入 git（`git ls-files cloudflare/backups` 为空），但任何同步/打包/误 add 即泄漏。建议立即轮换备份中出现过的全部 token |
| CF-13 | `web/src/index.ts:385-395` | `app_versions.download_url` 无协议白名单：可登记 `javascript:`/`file://`/钓鱼域，原样下发全部客户端弹更新窗（对照 `/api/rom` 有 `usableRomUrl` 校验） |
| CF-14 | `src/index.ts:526,618-647` + `wrangler.toml:23` | 心跳租约 3s 最小间隔是"用户级全局"谓词，与 `ONLINE_SESSION_CAP=3` 多设备语义自相矛盾：两设备心跳相位差 >3s 时第二台会话 CAS 永远失败 → 持续 429 → 120s 后被强制下线 |
| CF-15 | `src/crash-diagnostics.ts:8-16,48-130` | 匿名崩溃报告服务端零脱敏（设计自认），16 KB panic + 32 KB backtrace 任意文本可入库持久化 90 天；IPv6/代理轮换可持续填充 |

### 桌面端错误处理（1 条）

| # | 位置 | 问题 |
|---|---|---|
| INF-8 | `nwflash-infrastructure/src/auth.rs:151-159` | `validate_token` 把网络瞬断/限流/5xx 全折叠为 `Ok(None)`（未登录）：服务端不可用被当成凭据失效，触发无谓登出与降级。仅 401 应映射未登录，其余应传播 |

### （P2 计数：INF×6 + WIN×3 + CF×10 = 19）

---

## 三、P3 清单（26 条，概要）

**桌面端（16 条）**：
- INF-9 `root_resources.rs:101` `resolve_manager` 双候选校验失败仍 `unwrap_or(cached)` 返回未验证路径（调用方有二次 verify 兜底，语义不 fail-closed）
- INF-10 `vivo_firmware.rs:167` partial 固定名非 create_new，并发提取互相截断
- INF-11 `root_patch.rs:97` pending 固定名同上
- INF-12 `ota_download.rs:658` staging nonce 进程内计数器重启归零，路径可预测
- INF-13 `resource_downloader.rs:317` staging 可预测 + `File::create` 跟随预占位 symlink（远端资产有 sha256 硬门缓解）
- INF-14 `payload_provisioner.rs:144` `%TEMP%\VivoKsu\payload-dumper\{纳秒}` 可预测（exe 有 sha256 硬门缓解）
- INF-15 `preferences.rs:63` settings 固定名 tmp + 非 create_new + 失败残骸不清
- INF-16 `paths.rs:10` `resource_root()` 每调用重探针，结果可跨调用漂移
- INF-17 `api_client.rs:1038` `CloudflareClient::url()` 绝对 URL 直通分支（当前调用方全为常量，纵深防御缺口）
- INF-18 `usage_reporter.rs:128` 持锁全量序列化落盘 + persist 无原子替换
- WIN-6 `safe_flash.rs:579` fastbootd_wait_seconds 子秒轮询 `as_secs().max(1)` 导致等待秒数最高 4 倍虚报（进报错文案误导排障）
- WIN-7 `driver.rs:1090` 提权 pnputil 等待循环无取消注入点（UAC 期间点取消仍显示运行中）
- WIN-8 `mirror.rs:436` stale PID 复用窗口可误杀无关进程（注释 N16 已自认取舍）
- WIN-9 `device_transport.rs:17` fastboot flash 参数不拒绝前导 `-`（生产调用点全有白名单，纵深防御）
- WIN-10 `operation_coordinator.rs:1074` 时钟回退 epoch unwrap_or(0) 归零
- WIN-11 `commands/safe_flash.rs:128` settled Notify 注册竞态丢失窗口（有 300s 超时兜底，偶发多等）

**服务端（10 条）**：
- CF-16 `trace-v2-query.ts:902` afterChunk 无上界，`output_complete` 可误报 true（UI 有二次校验兜底）
- CF-17 `overview.js:47` vs `trace-v2-query.ts:1052` "今日"边界本地时区 vs UTC 差 8 小时，口径不一致
- CF-18 `web/src/index.ts:630` kick-by-user 硬编码 `affected:1`；writeAudit 失败静默吞
- CF-19 `schema.sql` + `src/index.ts:304` api_users/admin token 明文可检索存储（建议 SHA-256(token) 落库）
- CF-20 `trace-v2-contract.ts:199` credential_redactions.kind 客户端任意文本可污染计数
- CF-21 `trace-v2-redaction.ts:450` + `api.js:240` + `audit.js:1260` F-01 修复后死代码三段
- CF-22 `web/src/index.ts:562,686` `Number(userId)` NaN 直接绑定 → 500 而非 400
- CF-23 `security.ts:7` 固定窗口限流整窗对齐，切换瞬间 2× 配额
- CF-24 `web/src/index.ts:530` rotateUserToken/deleteUser/updateUser 无 admin_audit_log 审计
- CF-25 `web/src/index.ts:265` ensureAdminSeed 并发双插入竞态 + 每请求 COUNT

---

## 四、无发现项（专项验证通过）

- **pinned_tls.rs**：双重 SPKI pin + webpki 完整链验证 + host 白名单；pinset 更新 Ed25519 verify_strict + 版本单调防回滚 + 过期 fail-closed。无降级路径。仓库最强模块。
- **trace_spool.rs**：junction/symlink 防护、跨进程文件锁、有界读取、CAS 修订链完整。
- **zip slip**：payload_provisioner（含 Windows 保留名/ADS 拒绝）、scrcpy、firmware_extract 三处解压均无路径遍历。
- **第三方资产**：scrcpy/payload_dumper/manager APK 全部硬编码 sha256 + 长度双重 pin。
- **SecretToken 生命周期**：Zeroizing 贯穿、Debug REDACTED、Authorization sensitive 标记。
- **进程管道**：独立线程排空 stdout/stderr 进有界队列，8MB 输出无死锁；进程树 taskkill /F /T + 双确认 + 有限回收。
- **注入面**：serial/分区名/KMI/vendor_boot token/模块目录全部字符集白名单封死，无 shell 拼接。
- **跨设备变砖防护**：verify_execution_plan_device、备份序列号重校验、wait_for_fastbootd expected_serial 三处齐备。
- **编排状态机**：AdmissionGate 毒化 fail-closed、单许可信号量、epoch 互斥、ExitSupervisor exactly-once——无生命周期竞态。
- **SQL 全程参数化**（含 json_each 数组参数）；XSS 三层防护（createSafeElement + text-node + CSP）；跨用户 ownership 双层闭环。

---

## 五、跨线共性问题（修复策略依据）

1. **超时体系同根缺陷**（WIN-1/2/3/4/5，5 条）：命令超时预算依赖调用方显式传入，`None` 是默认值；`for_command` 字面参数匹配在引号脚本形态下系统性失效。建议统一原则：**executor 层默认必有超时，无超时需显式 opt-out 并注明理由**。
2. **固件完整性链缺口**（INF-1/2/3/4/5，5 条）：2f1edbb 只修了 ROOT 云提取一处；safe_flash 线刷链未接线、verify 是独立第二次下载（TOCTOU）、两个下载 client 无 scheme/proxy 约束。三条合成一条完整 MITM 换包路径。
3. **服务端鉴权与资源**（CF-1/2/3/4/5/11，6 条）：两个登录入口零限流、改密语义两处不对称、存储无配额。修复量小时级，风险最高。
4. **留存与脱敏**（CF-6/7/8/9/10/15，6 条）：时间戳无上界可永久绕过清理、PII 不在脱敏范围、ReDoS。

## 六、建议修复优先级

1. **批一（P1，9 条）**：CF-1/2（登录限流 + dummy PBKDF2，小时级）→ CF-3/4（改密语义）→ INF-1（safe_flash sha256 接线，复用 `RemoteFirmwareIntegrity`）→ WIN-2（`for_command` 上下文识别，防半写分区）→ WIN-1（SafeFlash 超时注入）→ CF-5（run/chunk 配额 + usage/logs 体限制）→ INF-2（reader 复用消 TOCTOU）
2. **批二（P2 高危）**：CF-12（**token 轮换 + 备份处置，独立于代码修复，建议立即执行**）→ INF-4/5（传输层收紧）→ CF-7（时间戳上界）→ CF-14（心跳 per-session）
3. **批三（其余 P2 + P3）**：按模块分批
4. **待确认设计决策**：INF-3（手动 URL 明文无门——有意决策，需产品裁决是否收紧）

——报告完。修复待批准；执行时严格按剔除清单，被剔除项不补修。
