# 客户端纵深加固实施记录（P0–P3）

日期：2026-09-21
范围：`src/Nwflash.Desktop/src-tauri/crates/{nwflash-domain,nwflash-protection,nwflash-application,nwflash-infrastructure,nwflash-windows,nwflash-tauri}`、`src-tauri/tauri.conf.json`、`src/app/`

## 0. 底线

**这是本地刷机工具。用户在写入分区时设备可能正处于 fastboot 会话中。**
任何时候都不得为了"防破解"而在刷机中途 panic / exit / 中止设备会话——
那会直接导致变砖。本记录里的每一处设计取舍都服从这条底线。

## 1. P0：Tauri IPC 边界

| 交付 | 落点 |
|---|---|
| 写类命令入口守卫 | `crates/nwflash-tauri/src/commands/guard.rs` |
| 守卫接入（14 个命令） | `partitions.rs`、`quick_flash.rs`、`files.rs`、`device.rs` |
| devtools 禁用 | `tauri.conf.json`（`"devtools": false`） |
| 防 IPC 重写 + 拦调试快捷键 | `src/app/webview-hardening.ts` |

### 为什么是「令牌类型」而不是宏

任务书建议用守卫宏。但**宏无法强制"第一行"**——宏只展开成代码，写在第几行
都合法，漏写也不会编译失败。因此改用必须构造的令牌类型 `WriteCommandAdmission`：
私有字段 + 唯一构造入口，需要它的函数没法凭空造。

### 与既有门禁的关系

`operation_coordinator::run_async` 内已有 `LocalProtectionGate` 做本地能力校验。
命令入口守卫**复用同一个** `ProtectionContext::admit_write_command()`，因此入口
校验与 `run_async` 内校验**结论必然一致**，不会出现两套判定打架。

入口再校验一次的价值：`run_async` 的保护是**间接**的——只要有写类命令不经过
协调器，就完全没有防护。入口校验让"忘记校验"变成显式可见的缺口。

### 错误文案为什么不塌缩成 `Unauthorized`

绝大多数拒绝是「租约过期，请重新登录」这类**可自愈**状态；而「未登录」与
「进程身份不匹配」的处置方式完全不同。混成一句 `Unauthorized` 会让用户在
刷机中途更迷茫。**安全语义（fail-closed、拒绝执行）完全一致，只有文案不同。**

## 2. P1：两段式反调试与关键判定混淆

| 交付 | 落点 |
|---|---|
| 反调试状态机 | `crates/nwflash-windows/src/anti_debug.rs` |
| 写入中途挂起闸门 | `crates/nwflash-protection/src/suspend_gate.rs` |
| 挂起接线 | `crates/nwflash-application/src/safe_flash.rs`（`execute_with_suspend_gate`） |
| 关键分区名混淆 | `crates/nwflash-domain/src/safe_flash.rs`（`obfstr`） |

### 决定集合是闭的

`AntiDebugDecision` 只有三个变体：`Proceed` / `RefuseService` / `SuspendAndWarn`。
**类型上不存在"退出/中止"这一类**，因此调用方不可能写出 `panic!` 或 `exit`。
有一条测试穷举两个维度钉死这个不变量。

| 触发条件 | 处置 |
|---|---|
| 写入前检出调试器 | `RefuseService`——此刻还没有数据写进设备，拒绝是安全的 |
| **写入中检出调试器** | `SuspendAndWarn`——**挂起等待，绝不中断设备会话** |

### 挂起与取消是两种东西

- `DomainError::UserCancelled`：用户主动中止，走收尾逻辑。
- `DomainError::WriteSuspended`：暂停推进，设备会话**保持原样**。

挂起检查在**每个命令边界**执行：当前命令若已开始就让它跑完——中断一条已发出
的 fastboot 命令比等它结束更危险。

### 与完整性终局的区分

既有的 `exit_supervisor` 完整性终局（镜像 CRC 被篡改）会立即退出，那是**正确**的：
镜像已不可信，继续写入才会变砖。反调试挂起完全不同——调试器不改变镜像正确性，
中断写入才是危险。两条路径不可混用。

### `obfstr` 覆盖范围与验证

受保护分区名（8 个系统分区 + 3 个 boot 类）与槽位后缀改为编译期加密，
二进制里不再出现明文，逆向者无法用 `strings` 直接读出保护表。

验证方式（独立构建，非推断）：加密串在 PE 中 **0** 次命中，明文对照串 **1** 次。

### 平台原语说明

Windows 用 `IsDebuggerPresent`（`windows-sys`），并与 VMP 遥测
（`VMProtectIsDebuggerPresent`）取**或**，两信号同源。

**Linux `ptrace(PTRACE_TRACEME)` 未实现**，这是刻意的：该调用是*被调试方主动
请求*成为被跟踪者的语义，用于自我防护时需自行 detach；而本项目只发布 Windows
（`tauri.conf.json` 的 `targets: ["nsis"]`）。写一个永远不被验证的非 Windows
实现是假的覆盖率。

虚拟机存在**不算**调试器：VM 检测只作遥测，且大量用户跑在虚拟化环境里，
误判会直接拒绝正常刷机。

## 3. P2：固件包验签与本地配置信任

| 交付 | 落点 |
|---|---|
| 通用验签原语 | `crates/nwflash-protection/src/local_artifact.rs` |
| 固件包验签门禁 | `crates/nwflash-application/src/safe_flash.rs` |
| 本地配置校验 | `crates/nwflash-infrastructure/src/preferences.rs` |

### 固件包：真正的安全边界

接入点在 `list_zip_images` 入口——**解压任何镜像之前**，本地与在线两条路径都覆盖。

- 要求旁挂 `<包>.sig`，签名覆盖**整包 SHA-256**。
- 在线路径从 `url + .sig` **同渠道**下载签名（否则等于用攻击者的公钥验证攻击者的包）。
- **缺签名一律拒绝**，不做"没有签名就放行"的降级——那等于把门禁变成可选项。

大包走流式摘要（`verify_sha256_digest`），不必整包读进内存。

### 本地 `settings.json`：为什么**不**用签名

任务书要求本地配置也验签。实测该做法在本架构下**逻辑不成立**：

客户端**没有签名私钥**。`SESSION_SIGNING_PRIVATE_KEY_PKCS8` 只存在于 Cloudflare
Worker secret，客户端全仓的 `signing_key` 引用**全部位于测试代码**。而
`settings.json` 是客户端自己写的，因此：

1. 工具写出的配置**必然**验签失败；
2. 用户每次设置 scrcpy 路径，重启后都会被清空（`toolpath_preference_roundtrip`
   测试可复现该回归）；
3. 且不带来安全收益——能改这个文件的人，本来就能改 exe、注入 DLL、读进程内存。

改用**平台信任**：配置须为常规文件（拒绝符号链接）、位于工具私有目录内
（`%LOCALAPPDATA%\VivoKsu`，按路径分量判定）、体积有上限。scrcpy 路径额外要求
绝对路径，但**不要求当前存在**——用户设备可能拔掉，若因此清空配置就是纯功能损失。

密码学验签保留给**分发物**（固件包），那才是签名能防住的场景。

## 4. P3：FFI 边界

### 前提不成立

- 全仓 `crate-type` / `cdylib`：**0 处**
- `#[no_mangle]` 导出：**0 处**
- `extern "C"`：仅 2 处，且都是**导入**（`flock`，Unix 文件锁），方向与任务书相反

**没有导出，就没有需要 `catch_unwind` 的边界。** 写入任务书描述的示例函数会
制造一个没人调用的死代码——那是伪装成加固的噪音。

### `strip = true` 不采纳

`strip` 会剥离符号，使 `scripts/vmp/verify-link-layout.ps1` 无法用 MAP 解析 8 个
marker，**等于废掉整个加壳防护**。这是硬冲突，不是偏好问题。

### `panic = "abort"` 保留

它已配置，且与任务书的 `catch_unwind` 要求**互斥**：`abort` 下 panic 不会 unwind，
`catch_unwind` 根本不生效。保留 `abort` 是更强的保证——进程直接终止，不存在
unwind 到未定义状态的风险。

### 实际落地：lint 门禁

`Cargo.toml` 的 `[workspace.lints]`：

```toml
[workspace.lints.rust]
unsafe_op_in_unsafe_fn = "deny"   # 防"整个函数是 unsafe 所以随便调"
unused_unsafe = "deny"            # 未使用的 unsafe 是理解偏差的强信号

[workspace.lints.clippy]
undocumented_unsafe_blocks = "warn"
```

各 crate 以 `[lints] workspace = true` 继承。**已用临时探针验证门禁以 error 级别
真实生效**（不是空配置），当前全仓 19 处 unsafe 全部满足。

## 5. 验证证据

| 检查 | 结果 |
|---|---|
| `cargo check --workspace --all-targets` | 通过，零警告 |
| `cargo test --workspace` | **零失败** |
| `vitest run webview-hardening.test.ts` | 8/8 |
| `obfstr` 生效 | 独立构建：加密串 PE 内 0 次，明文对照 1 次 |
| lint 门禁生效 | 临时探针触发 `error: unnecessary unsafe block` |
| 固件包验签生效 | 接线后 5 个既有 zip 测试立即被拒绝（证明门禁非摆设） |

## 6. 产物影响

本次改动涉及 **7 个 crate**，因此：

> **此前的 VMP 裸 exe 与加壳产物全部失效，最终需重新编译并重新固化加壳证据链。**

恢复发布的顺序不变，见 [VMP/签名运行手册](release/tauri-vmp-signing-runbook.md)。

## 7. 附带修复：`auth_contract` 既有红灯

`nwflash-infrastructure` 的 `auth_contract` 有 11 个失败（09-21 起存在）。
根因是 login mock 用 `body_json` 精确匹配请求体，而客户端会发送随机生成的
`request_nonce`，导致 wiremock 匹配失败并回退 404。

改用 `body_partial_json`。**这不是把校验放空**：已做反向实验——故意把
`request_nonce` 的期望值写成错误值，相关用例立刻失败，证明该字段确实参与匹配。

修复后 `auth_contract` 14/14，整个 workspace 零失败。