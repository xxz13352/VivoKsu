# 客户端防护复审报告（2026-09-21）

范围：`src/Nwflash.Desktop/src-tauri/crates/*`、`src-tauri/tauri.conf.json`、`cloudflare/src/*`
方法：**逐条对照源码复核 P0–P3 的每条主张**，不采信先前记录的结论。

## 0. 复审结论一句话

先前记录的防护**大方向成立、但有 9 个静默缺口**：8 个写类命令零入口守卫，
1 条刷写路径绕过反调试挂起闸门。**已全部修复并加上源码级契约测试**。

## 1. 复审发现的真实缺口（已修）

这些缺口的共同特征是**静默**：`cargo check` 通过、`cargo test` 通过、
`clippy` 通过。没有任何机制在"漏写守卫"时失败——这正是它们能存活的原因。

### 1.1 八个写类命令缺少入口守卫（高危）

`guard_write_command` 的价值完全取决于"是否被调用"。全仓 75 个
`#[tauri::command]`，此前只有 19 处守卫调用。逐条核对命令语义后确认以下
**8 个写类命令完全没有守卫**：

| 命令 | 后果 | 此前状态 |
|---|---|---|
| `root_run_automatic` | ROOT 全自动流程，**把修补后的镜像写进设备分区** | 零守卫 |
| `root_execute_patched_artifact_flash` | 执行修补镜像刷写 | 零守卫 |
| `root_patch_vivo_ksu` | 修补 boot 并落盘 | 零守卫 |
| `root_patch_official_vendor_boot` | 修补 vendor_boot | 零守卫 |
| `root_install_manager` | 安装 ROOT 管理器 | 零守卫 |
| `driver_reinstall` | 本机提权副作用（写系统驱动目录） | 零守卫 |
| `resource_install` | 下载并落地可执行组件到本机 | 零守卫 |
| `mirror_start` | 拉起本机进程 + 启动设备端服务 | 零守卫 |

**为什么"第二层"没能兜住**：`operation_coordinator::run_async` 内的
`LocalProtectionGate` 确实会拦截——但它只对**高危分类**生效
（`Rebooting`/`Installing`/`Transferring`/`Flashing`/`Mirroring`）。
而 `root.rs` 的修补类命令用的是 `OperationKind::Hashing`，
按分类**不需要**租约复检，于是两层都没有拦截。

修复：8 个命令全部补上入口守卫。

### 1.2 ROOT 全自动刷写绕过反调试挂起闸门（高危）

`SafeFlashExecutionService` 有两个入口：

- `execute(...)` —— **没有**反调试挂起检查
- `execute_with_suspend_gate(...)` —— 每个命令边界检查是否应挂起

VIVO 线刷走的是后者；而 `root_run_automatic` 的刷写阶段直接调
`execute(...)`。结果是**同一条设备写入路径上，走 VIVO 线刷会挂起、
走 ROOT 全自动却不会**。

把执行入口改成 `execute_with_suspend_gate` 后，参数个数不匹配会**直接编译失败**
——这比测试更强的保证：绕过挂门在类型层面就不成立。

修复：抽出 `during_write_suspend_query` 作为挂起查询的**唯一构造入口**，
两条刷写路径共用；`root_run_automatic` 改走挂门版本。

## 2. 复审确认成立的部分

| 主张 | 复核结果 |
|---|---|
| P0 令牌类型而非宏 | ✅ 成立。`WriteCommandAdmission` 字段私有，无处凭空构造 |
| P0 devtools 关闭 | ✅ `tauri.conf.json` `"devtools": false`、`withGlobalTauri: false`、CSP 存在 |
| P0 守卫与 `run_async` 共用同一 `ProtectionContext` | ✅ 成立，两者结论必然一致 |
| P1 决定集合是闭的 | ✅ `AntiDebugDecision` 仅 3 个变体，**类型上无退出分支** |
| P1 写入前拒绝 / 写入中挂起 | ✅ `decide()` 纯函数，4 条测试穷举两维度 |
| P1 VM 不算调试器 | ✅ 有专门测试钉死 |
| P1 `obfstr` 生效 | ✅ **独立实测**：受保护分区名在 PE 内 0 次命中 |
| P2 固件包验签 | ✅ 在 `list_zip_images` 入口、解压**之前**校验；缺签名 fail-closed |
| P2 本地配置用平台信任 | ✅ 理由成立（客户端无私钥，强制验签会自我失效且零收益） |
| P3 lint 门禁 | ✅ 6 个 crate 全部 `[lints] workspace = true` |
| `strip = true` 不采纳 | ✅ 成立。会剥离符号导致 8 个 marker 无法解析，废掉加壳 |

## 3. 新增的源码级契约测试

缺口能存活是因为**没有测试把"漏写"变成失败**。补两条：

### `tests/guard_coverage.rs`

- 每个写类命令体内必须出现 `guard_write_command`
- 全部命令必须显式分类（写类 / 只读），防止有人把写类命令挪进只读名单"让测试变绿"
- 同一命令不得同时出现在两个名单

**反向验证**：修复前该测试列出全部 8 个缺口并失败；修复后 3/3 通过。

### `tests/suspend_gate_coverage.rs`

- 任何刷写执行点必须调 `execute_with_suspend_gate` 而非 `execute`
- 反调试挂起判定只能在 `during_write_suspend_query` 里出现一次
  （内联写第二套判定意味着将来状态机改了它不会跟着改）

## 4. 验证证据

| 检查 | 结果 |
|---|---|
| `cargo check --workspace --all-targets` | 通过 |
| `cargo test --workspace` | **零失败**（含新增 5 条契约测试） |
| `cargo clippy --workspace --all-targets` | 仅既有 `undocumented_unsafe_blocks` warn（按设计） |
| 守卫覆盖契约测试 | 修复前 FAIL（列出 8 缺口）→ 修复后 PASS |
| 挂门覆盖契约测试 | PASS；把 `execute` 改回即编译失败 |
| 裸 exe 重建（`build_id 2026.09.21.5`） | 11,502,080 B，健康值（非 1.5 MB fat-LTO 残废形态） |
| `Assert-ExactVmProtectImports` | verified=True，8 符号 |
| `Assert-DesktopMarkerLayout` | verified=True，8 marker |
| 裸 exe 探针 | `exit_code=41`（正确态：已链接 SDK 未加壳） |

## 5. 对 VMP 加壳的影响

本次修改涉及**源码**，因此：

> **旧裸 exe 与旧交接记录全部作废，需按新哈希重新加壳。**

新交接参数（详见 `target/release/VMP-HANDOFF.txt`）：

- 输入：`nwflash-desktop.exe`，11,502,080 B
- SHA-256：`C19C8132CB2B163B031FF4A9300BBB4B927CF25E540066629BF540DCA12CFDFF`
- build_id：`2026.09.21.5`
- 公钥（编译期内嵌）：`HSNEfWZrjbZRhspVBhjcVOPxWiJJmx7tHO7JVMMug8o=`

加壳后 3 条验收：① 探针 `exit_code=0` 且三信号全 true ② 输出哈希 ≠ 输入
③ `dumpbin /IMPORTS` 无 `VMProtectSDK64.dll`。

## 6. 仍未闭合的开放项（已建议不动）

1. **P2 配置方案保持平台信任**：客户端没有签名私钥，强制验签会让工具自己
   写的配置必然失效。
2. **只读命令不加守卫**：不在"刷机/写分区/修改配置"范围内。
3. **`undocumented_unsafe_blocks` 保持 warn**：19 处 unsafe 全部有 SAFETY
   注释，升级为 deny 需要先补齐注释格式，与本次安全目标无关。
