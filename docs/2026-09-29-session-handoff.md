# 2026-09-29 会话接力（驱动安装根因修复 + VMP 交接链重跑）

本文件是本会话的权威交接点，自包含到可以不做任何前置阅读直接开工。上游背景（架构、
仓库结构、保护链）见 [project-architecture.md](project-architecture.md) 与
[2026-09-11-session-handoff.md](2026-09-11-session-handoff.md)、
[release/tauri-vmp-signing-runbook.md](release/tauri-vmp-signing-runbook.md)。

**本会话已全部提交并推送**：`93482a1`（`origin/master` 已同步），工作区干净。

## 1. 本次会话做了什么

用户依次提了三个问题，全部处理完毕：

1. **编译**（承接上一会话）——发现并修掉了 MAP 时间戳问题，跑通 protected release。
2. **驱动安装失败**（`pnputil 退出码 -536870325`）——定位真根因并修复。
3. **VMP 交接链重跑** + 本交接文档。

### 1.1 MAP「时间戳不正确」的根因（已修）

用户报 VMProtect 报 `File "nwflash-desktop.map" has an incorrect timestamp and cannot be loaded`。
**根因是裸 `cargo build --release` 造成的，不是工具链问题**：

- `/MAP:` 只在 **`protected` feature** 下由 `src-tauri/build.rs` 注入。
- 不带 `protected` 的 `cargo build --release` 会**重新链接 EXE 但不重生成 MAP**，
  于是 EXE 与 MAP 跨世代 → VMProtect 按 MAP 内部记录的时间戳比对后拒收。
- 实测对照：修复前 EXE `09-28 22:21:14` / MAP `09-27 18:31:03`；走 `protected`
  重链后 EXE `22:35:47.419` / MAP `22:35:47.430`（同一次链接，差 11ms），
  MAP 内 `Timestamp is 6aba7b42` 与 EXE 一致。

**结论**：发布/交接一律走 `Publish-TauriRelease.ps1 -PrepareManual`，**不要**裸
`cargo build --release` 污染 `target/release`。

### 1.2 驱动安装失败的真根因（已修，提交 93482a1 + a1e17fd）

报错：`pnputil 退出码 -536870325`。用户给出决定性线索：**同一台机器、同一个驱动包，
C# 版能装成功**。

> **本节结论被本会话内推翻并重写过一次**（09-29 上午先误判、下午修正）。
> 阅读时以本版为准；`93482a1` 里的注释与测试已由 `a1e17fd` 更正。

#### 最终结论：`0xE000024B` 不是失败

用户 09-29 第二次贴出现场日志后，逐字比对发现**该日志本身就是通配符 `/subdirs`
形态的输出**（决定性证据：包名是相对路径 `adbinfs_win10\android_winusb.inf`，
逐条形态会打印完整绝对路径）。即 09-29 上午的形态修复**已经生效**，而它**照样**
返回同一退出码。

本机用同一份 vivo 驱动包提权重跑，拿到**逐字一致**的输出与同一退出码：

```
Adding driver package:  adbinfs_win10\android_winusb.inf
Driver package added successfully. (Already exists in the system)
Published Name:         oem19.inf
...
Adding driver package:  fastboot_dri_win7\android_usb.inf
Failed to add driver package: The hash for the file is not present
in the specified catalog file. The file is likely corrupt or the victim of tampering.
...
Total driver packages:  8
Added driver packages:  7        ← 7 个成功
退出码: -536870325               ← 正常汇总码，不是失败信号
```

所以 `0xE000024B`（severity=3/facility=0，**CONFIGRET 域**不是 Win32；
`certutil -error` 查不到文本）只是「8 个包处理完、其中有包已存在或个别包被拒」时的
正常汇总码。真正的失败是 `fastboot_dri_win7` 的 **Win7 catalog 校验**不过——
与设备连接、与 vivo 驱动都无关。

**被推翻的错误归因**：上午曾把该码归给「逐条喂单个 INF 缺 catalog、设备绑定失败」，
并据此认定逐条形态必失败。**通配符形态同样返回该码**，归因不成立。命令形态保留
通配符 + `/subdirs`（形态本身没问题，且相对包名便于逐包归属），但**它是选择而非必需**。

#### 修正后的成功判定（用户 2026-09-29 定稿规则）

**按逐包结果分类，完全弃用退出码作为判据**：

| 类别 | 规则 |
|---|---|
| **分系统的遗留包**（目录段带 `win7`/`win10`：`adbinfs_*` / `fastboot_dri_*` / `mtk_cdc_*`） | 同一驱动的多系统版本，**装上一个就算成功** |
| **不分系统的驱动**（如 `mtk_FTDI-Driver`） | **必须全部成功** |
| 输出未回收（拿不到逐包条目） | **不得判成功**——没有证据不等于成功 |

按**目录段**判断而非整串包含，避免文件名里的偶然 `win10` 子串误判。

**解析要点**：`Failed to add driver package: <原因>` 行**不带包名**，必须与它前面那行
`Adding driver package: <路径>` 配对归属——解析器维护「当前正在处理的包」游标。

#### 落地的改动（3 个文件，两个 commit）

| 文件 | 改动 |
|---|---|
| `crates/nwflash-windows/src/driver.rs` | 命令形态为单条通配符 + `/subdirs`（`common_directory_root()` 定位通配符根）；新增 `DriverPackageEntry` 逐包解析 + `InstallOutcome::succeeded()` 分类判定；`exit_code` 降级为纯诊断字段；`driver_install_failure_detail` 点名具体失败包 |
| `crates/nwflash-tauri/src/commands/drivers.rs` | 失败判定改走 `driver_install_succeeded`（不再看 `exit_code != 0`） |
| `crates/nwflash-windows/tests/driver_installer.rs` | 替换基于错误结论的「一条命令一个 INF」契约测试；新增 4 用例：**真实日志逐字复刻**、不分系统包失败、分系统包全挂、输出未回收。`driver.rs` 单测重写为 6 用例覆盖分类规则与目录段判定 |

**保留的设计**：提权 batch 机制**没删**——命令数恒为 1，但 batch 仍是
「一次 UAC + 回收 pnputil 输出」的唯一通道（`ShellExecuteExW` 不给管道，输出只能经
状态文件 + 日志文件回收）。通配符是**自己拼的固定模式**（`<已校验解包根>\*.inf`），
不是外部输入。

**给 C# 版「能装成功」的解释**：C# 侧 `if (exitCode == 0) WriteAdbUsbIni()` 之后把
退出码回传，UI 未必把非零码弹成错误——所以「C# 成功」很可能只是**它没把这个非零码
当失败展示**，而不是命令形态更优。这也解释了为什么形态改动没能解决用户看到的现象。

### 1.3 验证结果（全绿，全部实跑）

| 项 | 命令 | 结果 |
|---|---|---|
| 驱动安装用例 | `cargo test -p nwflash-windows --test driver_installer` | **18 passed / 0 failed** |
| Rust 全量 | `env -u http_proxy … cargo test --workspace` | **1128 passed / 0 failed**（exit 0） |
| spawn 门禁 | `cargo test -p nwflash-windows --lib production_process_spawn_sites` | passed |
| clippy | `cargo clippy -p nwflash-windows -p nwflash-tauri --all-targets` | 0 error（warning 全为既有） |
| 前端类型 | `npx tsc --noEmit` | 零错误 |
| 前端单测 | `npx vitest run` | **261 passed / 28 files** |

**已知的既有噪声（非本次引入，别误判成回归）**：
- `cargo fmt --all --check` 在 `crates/nwflash-application/src/operation_coordinator.rs`
  报 2 处 diff（:94、:883 附近的换行风格）——**既有格式漂移**，本次未改该文件。
- `cargo clippy -- -D warnings` 会因 `driver.rs`（6 处）与 `file_ops.rs`（1 处）
  既有的 `undocumented_unsafe_blocks` 报 7 个 error。已用 `git stash` 对**未修改
  基线**验证过：同样 7 处、同样文件，只是行号偏移——**本次改动零新增 clippy 问题**。

## 2. 当前发布链状态（接续的第一件事）

**新 handoff 已就绪**：`artifacts/vmp-handoff/33c82bf8-3899-4050-9cf8-67ba4c6d026a`

```
handoff_id : 33c82bf8-3899-4050-9cf8-67ba4c6d026a
git_commit : 93482a16eaa1e73c12171ecadccd57525a06e612   ← 含驱动修复
build_id   : 2026.09.28.1
state      : prepared    （等待 VMProtect Lite GUI 处理）
input_exe  : FF4221915BB4277B40E179C487886B1B0C2375D06918873C329D0D1E65EBF9AD  (11,766,272 B)
input_map  : 687C34C25DFBC5AB4644172CD0500472DA33BE75152E4B06A4235F28CA8E04FC  marker_layout_verified=true
input_pdb  : matches_input_exe=true
输出路径   : artifacts\vmp-handoff\nwflash-desktop.vmp.exe
编译日志   : artifacts\vmp-handoff\compiler.log
```

**产物健康四联判据已核对**（防「编译成功但产物是残废探针」）：大小 11,766,272 B
（健康档 ~11.4MB / 残废档 ~1.5MB）、MAP 8/8 保护叶子、前端 bundle `index-DRY8zCgr`
命中、生产公钥 base64 文本命中、`/subdirs` 命中（证明驱动修复已编入）。

**VMProtect GUI 已打开**并加载了上述 staged EXE（若已关闭，见 §4 重启命令）。

### 2.1 待用户手工完成的一步（设计上禁止自动化）

`Protect-NwflashRelease.ps1` 直接 `throw`——仓库**故意禁用** `VMProtect_Con.exe`
脚本化保护，只走 GUI 手工交接。需按契约配置：

**保护选项**：Memory Protection ✓ / Import Protection ✓ / Packing ✓ /
**VM 执行拒绝 ✗**（保持关闭；调试器与 VM 检测只作遥测信号，不得触发进程退出、不得在
设备操作期间轮询）

**8 个标记的模式**（模式错会被 accept 链拒收）：

| 符号 | 模式 |
|---|---|
| `nwflash_protection_accept_login_lease` | Ultra |
| `nwflash_protection_admit_local_operation` | Ultra |
| `nwflash_protection_requires_protected_recheck` | Ultra |
| `nwflash_protection_trace_credential_sentinel` | Ultra |
| `nwflash_protection_terminate_process` | Ultra |
| `nwflash_protection_classify_heartbeat_lease` | Virtualization |
| `nwflash_protection_verify_image_integrity` | Virtualization |
| `nwflash_protection_build_identity_matches` | Mutation |

不要扩大标记范围到 Tauri/WebView 入口、async 状态机、HTTP/TLS、adb/fastboot、驱动、
子进程控制、下载、解压、固件写入或第三方代码。标记输入是固定标签，**永不**包含
口令、令牌、路径、URL 或设备序列号。

输出到（契约指定路径，**不得**覆盖原未保护 EXE）：
`artifacts\vmp-handoff\nwflash-desktop.vmp.exe`；
编译日志存到：`artifacts\vmp-handoff\compiler.log`。

### 2.2 处理完之后的接续命令

```bash
pwsh -NoProfile -File scripts/vmp/accept-manual-output.ps1 \
  -PreparedManifest "artifacts\vmp-handoff\33c82bf8-3899-4050-9cf8-67ba4c6d026a\evidence\prepared.json" \
  -MarkerReviewPath "<marker-review.json 路径>"
```

accept 链会拿 EXE / MAP / compiler.log 三者哈希做交叉核对，然后进入签名、NSIS 打包、
装机比对。后续 `Publish-TauriRelease.ps1 -AcceptedEvidence …` 需要
`NWFLASH_CERT_THUMBPRINT`（40 位 SHA-1）。

## 3. 换电脑继续：环境复现清单

本会话所有编译/交接命令都依赖一组环境变量。**新机器上照抄 §3.1 即可**。

### 3.1 环境变量（每次新 shell 都要设）

```bash
export CARGO_INCREMENTAL=0
export PATH="/c/Program Files (x86)/Microsoft Visual Studio/2022/BuildTools/VC/Tools/MSVC/14.44.35207/bin/Hostx64/x64:$PATH"
export LIB='C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Tools\MSVC\14.44.35207\lib\x64;C:\Program Files (x86)\Windows Kits\10\Lib\10.0.26100.0\um\x64;C:\Program Files (x86)\Windows Kits\10\Lib\10.0.26100.0\ucrt\x64'
export INCLUDE='C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Tools\MSVC\14.44.35207\include;C:\Program Files (x86)\Windows Kits\10\Include\10.0.26100.0\ucrt;C:\Program Files (x86)\Windows Kits\10\Include\10.0.26100.0\um;C:\Program Files (x86)\Windows Kits\10\Include\10.0.26100.0\shared'
export NWFLASH_VMP_SDK_ROOT='C:\Users\17254\Downloads\VMProtect Lite v3.10.4 Build 2668 (1)'
export NWFLASH_BUILD_ID='2026.09.28.1'
export NWFLASH_SESSION_VERIFY_KEY_B64='HSNEfWZrjbZRhspVBhjcVOPxWiJJmx7tHO7JVMMug8o='
export NWFLASH_DUMPBIN_PATH='C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Tools\MSVC\14.44.35207\bin\Hostx64\x64\dumpbin.exe'
```

要点：
- `PATH`/`LIB`/`INCLUDE` 三件套缺一不可——只给 `LIB` 时 C 构建脚本
  （zstd-sys / liblzma-sys / ring）会失败；只给 `INCLUDE` 时链接器会解析到
  Git Bash 的 coreutils `link`（报误导性的「install Visual Studio build tools」）。
- **`NWFLASH_VMP_SDK_ROOT` 目录名带 ` (1)`** ——本机实际路径就是
  `VMProtect Lite v3.10.4 Build 2668 (1)`（旧笔记写的无后缀名在本机不存在；
  `ls ~/Downloads | grep -i vmprotect` 现查）。
- `NWFLASH_SESSION_VERIFY_KEY_B64` 与 `NWFLASH_BUILD_ID` 是 **`option_env!` 编译期**
  变量。缺任一个：release 下 `AppState::try_new` 必失败 → fat LTO 把整个应用主体
  （命令注册表、登录/心跳链、8 个保护叶子）当死代码裁掉，产物只剩 ~1.5MB 的探针路径，
  **过程完全不报错**。必须用 §1.3 的四联判据验产物。
- 可用 `-j 4` 限并发规避 C 构建脚本的随机竞争失败。

### 3.2 新机器必须自备的外部依赖

本仓库**不含**这些，需自行准备并保持哈希一致：

| 依赖 | 版本 | 校验值（`scripts/vmp/verify-sdk.ps1` 钉死） |
|---|---|---|
| VMProtect Lite x64 header | v3.10.4 Build 2668 | `2300B7B4BB6BBF9CFA08013EC2D9B2FDCEB3DFD2E603CD1E24A493DE4D165B15` |
| 导入库 `VMProtectSDK64.lib` | 同上 | `9997A9C6E179010450385832A66EA36938E180FC9067D91FD6AAE7C9F6BF4D18` |
| SDK DLL `VMProtectSDK64.dll` | 同上 | `EC3235136A4DAEE2A6F72C0F2994A8365CA8427C8068D068130B74C9FA64CD02` |
| MSVC BuildTools | 14.44.35207 | — |
| Windows SDK | 10.0.26100.0 | — |
| rustc / cargo | 1.98.1 | — |
| Node | 24.18.0 | — |
| PowerShell | `pwsh` ≥ 7.4（**Windows PowerShell 5.1 不支持**，`#requires` 会直接失败） | — |

SDK 三件套哈希不符会被**直接拒收**（「A structurally similar or newer SDK is rejected
until those release pins are deliberately reviewed」）。SDK 与 license **不得**拷进仓库。

### 3.3 交接产物如何跨机传递

`artifacts/` 在 `.gitignore` 里，**不会随 git 走**。跨机继续有两条路：

- **推荐**：在新机器上重新跑 §4 的 `-PrepareManual`（前置检查全自动，约 10 分钟），
  产出该机器自己的新鲜 handoff。**注意 `prepared.json` 绑 git_commit 与绝对路径**，
  直接拷目录过去会因路径不同而失效。
- 若必须传：整个 `artifacts/vmp-handoff/<handoff_id>/` 目录一起拷，且新机器路径要与
  `prepared.json` 里的绝对路径一致，否则 accept 链的路径校验会拒。

## 4. 常用命令（本会话实测可用）

重跑 VMP 交接准备（约 10 分钟，含 protected release 全量重编）：

```bash
ROOT="C:\\Users\\17254\\Desktop\\存档\\TOOL\\VivoKsu 工具"
pwsh -NoProfile -File scripts/Publish-TauriRelease.ps1 -PrepareManual \
  -ProtectedOutputPath "$ROOT\\artifacts\\vmp-handoff\\nwflash-desktop.vmp.exe" \
  -CompilerLogPath "$ROOT\\artifacts\\vmp-handoff\\compiler.log" \
  -HandoffRoot "$ROOT\\artifacts\\vmp-handoff"
```

**必须传绝对路径**——相对路径会被
`Get-NormalizedFullPath` 拒（`Path must be fully qualified`），且该失败发生在
release 编译**之后**，白等一次全量编译。

重启 VMProtect GUI 并加载最新 staged EXE：

```bash
MSYS_NO_PATHCONV=1 powershell -NoProfile -Command "Start-Process -FilePath 'C:\Users\17254\Downloads\VMProtect Lite v3.10.4 Build 2668 (1)\VMProtect.exe' -ArgumentList '\"C:\Users\17254\Desktop\存档\TOOL\VivoKsu 工具\artifacts\vmp-handoff\33c82bf8-3899-4050-9cf8-67ba4c6d026a\input\nwflash-desktop.exe\"'"
```

只验 SDK / 链接契约（不编译，快）：

```bash
pwsh -NoProfile -File scripts/vmp/verify-sdk.ps1 -SdkRoot "$NWFLASH_VMP_SDK_ROOT" -AsJson
pwsh -NoProfile -File scripts/vmp/test-contracts.ps1 -SdkRoot "$NWFLASH_VMP_SDK_ROOT" -AsJson
```

Rust 全量测试（**必须 unset 代理**，否则回环测试假失败）：

```bash
env -u http_proxy -u https_proxy -u HTTP_PROXY -u HTTPS_PROXY -u all_proxy -u ALL_PROXY \
  cargo test --workspace
```

## 5. 未决事项

1. **`build.rs` 静默污染 MAP 的守卫**（本次踩到的坑，我提议过但用户未拍板）：
   `build.rs` 只在 `protected` 下写 `/MAP:`，但裸 `cargo build --release` 会静默把
   EXE/PDB 换成新一代、把 MAP 留在原地 → VMProtect 报时间戳不一致。可在 `build.rs`
   加「release 且已有 MAP 却不带 protected 时给 warning」的守卫。
2. **旧 handoff 清理**：`artifacts/vmp-handoff/` 下现在有三个——
   `53630471`（绑 `a0069bc`，最旧）、`130a6e3f`（绑 `fa98ee4`，已被取代）、
   `33c82bf8`（绑 `93482a1`，**当前有效**）。前两个已过期，确认后可删。
3. **`operation_coordinator.rs` 格式漂移**（2 处）——既有问题，未修；要顺手修可单独提 commit。
4. **7 处既有 `undocumented_unsafe_blocks` clippy error**——既有问题，未修。

## 6. 操作规则（合并沉淀，接续会话必读）

1. 审计工作模式：审计→报告→批准后修复；修复严格执行用户剔除清单，被剔除项不补修。
2. UI 改版先出 HTML 预览稿过目再实施；验证 `tsc` + `test:ui`。
3. 子代理并发 ≤3 且用后台模式；更多并发会被整批取消。
4. C# 归档（`archive/csharp`）是**行为真源**——本次驱动修复就是照它改的；`cloudflare/`
   是服务端真源；Rust 六层 crate 见 project-architecture.md。
5. 同一仓库**不要开两个会话同时改**；改码只用 Read+Edit，禁止 `git checkout/restore/stash`
   覆盖工作区。
6. `-PrepareManual` 要求工作区**干净**（`AssertGitClean`）——先提交再跑。
7. 打补丁/交接一律走 `-PrepareManual`，**不要**裸 `cargo build --release`。
