# NWFlash 发布门禁审计（更新至 2026-09-05）

## 结论先行

当前源码不能宣称“可发布”。主线功能提交已经合入，但当前只具备源码/部分自动化门禁证据；受保护 EXE、代码签名、NSIS 安装器、安装卸载、真机和生产部署证据均未形成一条可核验的完整链。

本报告来自只读发布审计；2026-09-05 文档卫生仅更新其中的当前事实。没有修改源码、配置、分支/ref、stash 或 artifact，没有部署、签名、运行 VMProtect 或连接真机。

## 1. 审计快照

- 工作区：C:/Users/17254/Desktop/存档/TOOL/VivoKsu 工具
- 分支：codex/vmp-release-completion
- 文档卫生开始时 HEAD：a27a843（chore(workspace): finalize relay plans and cleanup）。
- origin/codex/vmp-release-completion：860e728；此时本地领先 31 个提交、落后 0 个，发布前须重新记录精确差异。
- 本地分支目前只有 codex/vmp-release-completion 与 codex/integration-staging；没有注册的 side worktree。
- 文档卫生开始时源码/配置无未提交改动，另有两个独立 owner 的未追踪计划；当前文档清理本身会形成预期 diff。发布脚本不强制检查 Git clean，但在最终状态未冻结前不能把构建称为可复现发布构建。
- stash 仍有三项在途备份：stash@{0} Safe Flash、stash@{1} refresh-spawn、stash@{2} sealed-spool；不能删除或覆盖。
- 已用 `git bundle verify` 确认恢复 bundle：`C:\Users\17254\Desktop\VivoKsu-quarantine\branch-backup-20260905-v4.bundle`（68,024,704 字节）。它记录完整历史和 33 个 refs，HEAD=`a27a843`，包含两个本地 heads、全部 remote-tracking refs、`refs/stash` 与 `ORIG_HEAD`。本轮未提交文档卫生 diff 不在其中；形成新提交后再按需刷新。

审计期间其他会话可能继续提交；真正执行门禁前须重新记录 HEAD、status 和远端差异，并把本报告的历史快照与新快照分开。

## 2. 已合入的近期变更（仅证明代码落地，不等于发布通过）

当前提交链包含以下与发布相关的变更：

| 提交 | 内容 | 发布含义 |
|---|---|---|
| 604ade3 | Safe Flash 分区失败暂停/继续/中止弹窗 | 功能已落地；需要在当前 HEAD 重跑应用、Tauri 和 UI 回归 |
| e5102cc | nwflash-tauri 测试宿主 Common Controls v6 manifest | 解除 0xc0000139 测试启动阻断；只证明测试宿主，不证明发布 EXE |
| 82cc192 | 文件页取消入口 | UI 取消路径已改；需重跑页面和端到端验证 |
| 8f759ef | 文件传输临时目标/原子收尾 | 变更面大；现有旧验证报告在修复前，不能直接作为当前证据 |
| 741ef97、819cf25 | 管理后台导出完成协议与流式导出校验 | 需重跑 admin unit/Workerd/Chromium 全套 |
| 59fe97d | 管理后台用户 token 响应校验 | 需重跑 token mutation、401/畸形 2xx 矩阵 |
| 2d865da | workspace clippy 窄 lint 修复 | 需要当前 HEAD 的完整 fmt/clippy 结果 |
| e17bd81 | API 版本头 fail-closed 实现与文档 | Node 77、Workerd 203、Rust contract 28 通过；仍需部署前兼容确认 |
| f6b7dd7 | 文件传输 P1 后验证 | 保存分层 mock/定向门禁与剩余 P1-03/P2 边界 |
| a27a843 | 接力计划和发布审计收口 | 当前文档/发布入口基线 |

已存在的文件传输、Web API、Safe Flash 和 Tauri manifest 报告跨越多个较早快照。它们可作为审计轨迹，不能替代当前 HEAD 的重新验收。

## 3. 当前产物与外部工具现场

### 3.1 产物现场

- src/Nwflash.Desktop/src-tauri/target/release/nwflash-desktop.exe：不存在。
- src/Nwflash.Desktop/src-tauri/target/release/bundle/nsis：不存在。
- 仅发现一个旧的 src/Nwflash.Desktop/src-tauri/target/release/nwflash-desktop.vmp.exe（2026-09-02 时间戳）；没有与当前 HEAD 绑定的 prepared/accepted/evidence sidecar，不能作为当前受保护发布输入。
- 仓库根 artifacts/ 目录不存在；没有 release root、SHA256SUMS.txt、exe-signed、nsis-built、installer-signed、installed-verified 或 release-verified 证据文件。
- target/debug 和 target/release 中的 Cargo 缓存不等于发布产物；不要从缓存文件名推断签名、VMP 或安装器状态。

### 3.2 本机可见工具

只读环境检查结果：

| 项目 | 现场 | 影响 |
|---|---|---|
| PowerShell | 7.6.5 | 满足脚本 7.4/Core 要求 |
| Cargo/Rust | 1.98.1 | 可运行 Rust 门禁 |
| cargo-fmt/cargo-clippy | 可发现 | 可运行 fmt/clippy，但尚无当前 HEAD 全量结果 |
| Node/npm | Node 24.18.0、npm 11.16.0 | 可运行前端/Cloudflare 门禁 |
| desktop node_modules | 存在 | 桌面 UI/build 可运行 |
| cloudflare/node_modules | 存在 | Cloudflare 本地门禁可运行 |
| cloudflare/user/node_modules | 存在 | 用户门户门禁可运行 |
| desktop e2e-tests/node_modules | 不存在 | native WDIO 尚不能运行，需锁文件对应的 npm ci |
| Windows SDK、vswhere、NSIS 路径 | 文件存在，但不在 PATH | 发布脚本可尝试自动定位；需实际门禁确认 |
| signtool/dumpbin 全局命令 | 不在 PATH | 脚本通过 vswhere/SDK 路径定位；仍需实机验证 |
| NWFLASH_VMP_SDK_ROOT | 未设置 | 受保护构建、SDK 校验和 link-layout 尚不能执行 |
| NWFLASH_VMP_PATH / ARGUMENTS | 未设置 | 仅手工 VMP 交接，自动 console 入口被脚本禁用 |
| NWFLASH_CERT_THUMBPRINT | 未设置 | Authenticode 签名不能开始 |
| VMProtect Lite GUI/项目/许可证 | 仓库外且当前未提供可核验路径 | 需要受控外部环境和人工操作 |
| Cloudflare 生产 token/账号 | 未在本地环境提供 | 不能验证远端迁移、secret 或部署 |

不应为了让门禁“变绿”而复制系统 DLL、把 SDK/证书放进仓库、写入生产 secret，或使用未核验的旧 EXE。

## 4. 发布脚本实际门禁

### 4.1 非部署的准备阶段

scripts/Publish-TauriRelease.ps1 的 PrepareManual 参数集会先执行：

1. PowerShell 7.4/Core 入口和脚本语法边界。
2. 外部 VMProtect SDK 的固定版本、头文件、AMD64 import library、SDK DLL 和八个导出校验。
3. VMProtect link/layout、六个 marker、MAP/dumpbin 和 source contract。
4. 前端 capability、Rust protection probe、Tauri release_probe。
5. 前端 production build。
6. 带 protected feature 的 Tauri no-bundle release build。
7. 解析 release EXE、PDB、MAP，生成不可变的手工交接目录。

当前只有第 4 项中的部分历史报告；没有当前 HEAD 的完整 PrepareManual 产物。由于 SDK 环境变量未设置，此阶段现在会在外部 SDK 预检处停止。

### 4.2 人工 VMProtect 阶段

scripts/Protect-NwflashRelease.ps1 的实现明确直接抛出“Automated VMProtect console execution is disabled”。受控 operator 必须：

- 使用审阅过的 VMProtect Lite 3.10.4 Build 2668；
- 对准备目录中的精确 unsigned EXE 执行六个同步叶子的既定保护模式；
- 保持 Memory Protection、Import Protection、Packing；不启用虚拟机拒绝；
- 输出到不同文件，确认非空且 SHA-256 与输入不同；
- 保存 compiler log 和 marker review；
- 通过 accept-manual-output.ps1 形成 accepted.json。

仓库中的 fixture 测试可以验证证据链规则，但不能证明 Lite GUI 实际运行，也不能证明 protected PE 含有六个物理保护区域。

### 4.3 签名、打包和安装阶段

FinalizeManual 只有在 accepted.json 通过完整 hash-bound evidence chain 后才会：

1. 将 protected output 复制到新的 packaging root，并用 Sign-NwflashRelease.ps1 做 EXE Authenticode 签名。
2. 在新的 Cargo target root 中重新生成 NSIS（不复用旧 bundle）。
3. 对唯一 installer 签名并验证 RFC3161 时间戳。
4. 用 Test-TauriInstaller.ps1 在受控临时目录静默安装、检查 EXE/资源/签名/精确文件集，然后静默卸载并确认目录消失。
5. 运行 Verify-ProtectedRelease.ps1，生成 release-verified，再生成并复核 SHA256SUMS.txt。

这条链依赖证书 thumbprint、x64 SignTool、NSIS、安装权限和可验证的受保护输出；当前没有任何一项的最终证据。旧的 target/release/nwflash-desktop.vmp.exe 不能跳过这些步骤。

## 5. 已有自动化证据与必须重跑的门禁

### 5.1 已有但属于历史快照的证据

- Tauri manifest 报告记录过 nwflash-tauri unit 300/300、mirror_runtime 1/1、release_probe 4/4，以及 mt.exe 提取 Common Controls v6；这是测试宿主证据，不是 release EXE、签名或安装器证据。
- 文件传输初始报告记录过 React、application、windows transport、process 和 coordinator 的定向通过；后续 P1 报告已覆盖 82cc192 与 8f759ef 的 mock/定向验证，但 native WDIO、Android shell 与 workspace flaky 仍未关闭。
- Web API 报告记录过 Node 77、Workerd 192、admin unit 135、admin Workerd 51、Chromium 31；随后合入 741ef97、819cf25、59fe97d，旧数字不能视为当前 HEAD 的结果。
- Safe Flash 的旧会话证据记录过 application/UI 通过；604ade3 已合入，但当前 HEAD 仍应统一重跑 workspace、Tauri、UI 和 production build。
- API 版本门禁在 e17bd81 已实现并通过 Node 77、Workerd 203、Rust API/version contract 28 及 typecheck/dry-run；尚未部署，旧无头客户端的 400 升级体验仍需发布确认。
- 根 PROJECT_PROGRESS.md 已重写为简洁当前入口；发布事实仍以本报告、实际 Git 状态和新一轮门禁输出交叉核对。

### 5.2 当前可在本机执行的非破坏性门禁

以下命令只做本地编译、测试、fixture、dry-run 或临时目录验证；会产生可再生成的缓存/临时文件，但不应接触真实设备、生产数据库或签名密钥。应在冻结工作区、确认 owner 已收敛后按顺序执行：

    pwsh -NoLogo -NoProfile -NonInteractive -File scripts/Test-PowerShellRuntimeBoundary.ps1
    cargo fmt --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --all -- --check
    cargo check --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --workspace --all-targets
    cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --workspace
    npm --prefix src/Nwflash.Desktop run test
    npm --prefix src/Nwflash.Desktop run build
    npm --prefix src/Nwflash.Desktop run tauri -- build --no-bundle
    pwsh -NoLogo -NoProfile -NonInteractive -File scripts/Test-TauriRelease.ps1
    pwsh -NoLogo -NoProfile -NonInteractive -File scripts/Test-ProtectedBuildProfile.ps1
    pwsh -NoLogo -NoProfile -NonInteractive -File scripts/Test-ProtectedRelease.ps1
    pwsh -NoLogo -NoProfile -NonInteractive -File scripts/Test-TauriCapabilityBoundary.ps1
    npm --prefix cloudflare test
    npm --prefix cloudflare run test:workerd
    npm --prefix cloudflare run test:admin:unit
    npm --prefix cloudflare run test:admin:workerd
    npm --prefix cloudflare run typecheck
    npm --prefix cloudflare run dry-run:api
    npm --prefix cloudflare run dry-run:web
    npm --prefix cloudflare/user test
    npm --prefix cloudflare/user run typecheck
    git diff --check

Test-TauriInstaller.ps1、Publish-TauriRelease.ps1 的 DevelopmentUnsigned 参数和 native WDIO 会创建/运行更多本地产物；它们仍不部署，但应单独确认输出根为空、路径受控、没有其他会话使用目标后再运行。

### 5.3 必须在当前 HEAD 重新记录的专项矩阵

- Safe Flash：application safe_flash 定向测试、Tauri safe_flash lib 测试、PartitionFailureDialog 测试、桌面 UI 全套、tsc、production build。
- 文件传输：FileManagerPage、application file_manager/file_transfer、windows file/process、Tauri files 命令测试；确认取消、临时目标、失败清理和 coordinator release。
- Tauri 宿主：nwflash-tauri lib 全套、两个 integration tests、workspace all-targets；确认每个新测试 EXE 的 RT_MANIFEST。
- Cloudflare admin：最新 users/audit 单元、Workerd、Chromium、typecheck/dry-run；旧报告的 F-01/F-02 结果不能直接复用。
- 用户门户和 API：Node/Workerd/UI 与跨 Worker D1 流程；版本门禁已实现，但部署兼容和独立方法/缓存问题仍不得误称为已完成。
- Native WDIO：在 e2e-tests 执行锁文件对应的 npm ci，运行 build-native-e2e.ps1 和全部 native/visual spec；禁止把浏览器 mock 代替真实 Tauri 进程证据。

## 6. 尚未验证或需要单独授权的发布步骤

| 阶段 | 当前状态 | 为什么不能由本机源码测试替代 |
|---|---|---|
| 未保护 release build + PDB/MAP | 当前 HEAD 尚无可核验产物 | 需要真实 release 编译、唯一 target root、版本/资源快照 |
| VMProtect SDK/link contract | 环境变量和外部 SDK 未提供 | 需要固定 SDK 文件、x64 dumpbin 和真实 COFF/PE 检查 |
| VMProtect Lite GUI | 未执行 | 人工 GUI、外部项目/license；仓库自动 console 明确禁用 |
| protected runtime/CRC/import removal | 未执行 | 必须运行 VMP 后 EXE 的机器可读 probe，并核对八个 SDK import 已移除 |
| EXE Authenticode 签名 | 未执行 | 需要授权证书、thumbprint、x64 SignTool、RFC3161 时间戳 |
| NSIS bundle | 当前 release/bundle/nsis 不存在 | 只能从签名 EXE 在新 packaging root 生成，不能复用旧目录 |
| 安装/卸载 smoke | 未执行 | 会运行 installer、写入临时安装目录并验证签名/文件树，需受控主机授权 |
| 真机设备矩阵 | 未执行 | 需要设备所有者批准、专用可恢复设备、备份和恢复演练；mock 不等价 |
| 驱动安装 | 未执行 | 会修改 Windows driver store，需要 VM 快照或可回滚测试主机 |
| Cloudflare D1 migrations/deploy | 未执行且禁止本卡执行 | 需要远端账号、D1 recovery point、生产 secret、域名和回滚授权 |
| 外部资源上传 | 未执行 | Upload-Resources.ps1 会写 GitHub release，属于外部状态变更 |
| Plan C 生产 trace 接线 | 文档仍称若干 producer/observer adapter 未接入 | 若产品把 V2 完整审计列为发布条件，必须先完成接线和崩溃/七天丢失矩阵 |
| API 版本头 fail-closed | e17bd81 已实现、未部署 | 发布前确认旧客户端覆盖、400 升级体验和 `/api/online` 豁免取舍 |

## 7. 建议执行顺序

1. 停止并行写入，收敛当前独立计划与文档卫生 diff；保存 stash，不做 broad clean。
2. 刷新并校验外部 Git bundle，记录当前 HEAD、远端差异、分支、stash 和工作区状态。
3. 在固定当前 HEAD 上跑第 5.2 的本地 Rust、前端、Cloudflare、fixture 和 all-targets 门禁；失败先修复并重新记录，不进入外部发布阶段。
4. 安装/确认 rustfmt、clippy 和 e2e-tests 依赖，完成 native WDIO 与视觉验收；保存测试退出码和 task-scoped 输出。
5. 在无真实设备的情况下完成 PrepareManual：SDK 校验、link/layout、protected no-bundle build、EXE/PDB/MAP handoff；核对 source reachability 与六个 marker。
6. 经明确授权后由 release operator 在隔离主机执行 VMProtect Lite GUI，保存 compiler log、marker review、distinct protected output，并运行 accept-manual-output。
7. 重新执行 protected runtime/CRC/import 验证，再签名 EXE；随后在全新 packaging root 构建并签名 NSIS，运行安装/卸载 smoke。
8. 生成 release-verified 和最终 SHA256SUMS.txt，运行 Verify-ProtectedRelease -RequireManifest；确认发布树不含 PDB、MAP、SDK、compiler log、未保护 EXE 或额外文件。
9. 取得设备所有者和运维授权后，按 device-acceptance-matrix 执行最小真机 smoke，并记录恢复结果。
10. 单独建立 Cloudflare D1 recovery point，依序执行 base → P0 → retention migrations、API/schema、admin UI 部署和合成 trace smoke；每一步保留回滚停止条件证据。生产部署不应与桌面签名步骤混成一个无审计命令。

## 8. 最终发布验收命令（外部步骤完成后）

以下是最终 operator 需要留档的命令形状；本次审计没有执行它们，也不构成授权：

    pwsh -NoLogo -NoProfile -NonInteractive -File scripts/Publish-TauriRelease.ps1 -PrepareManual -ProtectedOutputPath <external-protected-exe> -CompilerLogPath <external-compiler-log> -HandoffRoot <handoff-root>
    pwsh -NoLogo -NoProfile -NonInteractive -File scripts/vmp/accept-manual-output.ps1 -PreparedManifest <prepared.json> -MarkerReviewPath <marker-review.json>
    pwsh -NoLogo -NoProfile -NonInteractive -File scripts/Publish-TauriRelease.ps1 -FinalizeManual -AcceptedEvidence <accepted.json> -ReleaseRoot <final-release-root>
    pwsh -NoLogo -NoProfile -NonInteractive -File scripts/Verify-ProtectedRelease.ps1 -ReleaseRoot <final-release-root> -AcceptedEvidence <accepted.json> -ExeSignedEvidence <exe-signed.json> -NsisBuiltEvidence <nsis-built.json> -InstallerSignedEvidence <installer-signed.json> -InstalledVerifiedEvidence <installed-verified.json> -ExpectedThumbprint <40-hex> -RequireManifest

命令中的尖括号值必须由受控 operator 用实际、已核验的路径替换；不能把示例路径、旧 target 产物或 fixture evidence 当作生产输入。

## 9. 发布判定

当前判定：BLOCKED / NOT RELEASE READY。

阻塞不是因为源码提交缺失，而是因为以下证据链尚未闭合：当前 HEAD 的完整非部署回归、release unprotected build、固定 SDK/link 证据、真实 VMProtect Lite 处理、protected runtime/CRC、EXE 与 installer 签名、NSIS 安装卸载、批准的真机矩阵，以及 Cloudflare 迁移/部署授权与 smoke 记录。满足这些条件前，任何“source-ready”“gate-ready”或旧报告中的测试数字都不能改写成“已发布”或“可默认下载”。
