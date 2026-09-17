# NWFlash/VivoKsu 接力迭代总计划

更新时间：2026-09-05（Asia/Shanghai）

## 目标与工作方式

本计划是当前工程接力的执行入口。每项工作按“计划与验收条件 → 只读现状 → 单一 owner 实现 → 定向测试 → 受影响完整门禁 → 缺陷复查 → 总指挥审核”的顺序推进。

未经单独授权，不推送、部署、签名、运行 VMProtect、安装驱动或连接真机。产品范围继续遵循 [产品决策](product-decisions.md)：不把运行时哈希或跨步骤设备 serial 绑定作为新增功能目标。

## 当前基线

- 主分支：`codex/vmp-release-completion`。
- 文档卫生开始时 HEAD：`a27a843`；后续任务开始前必须重新记录实际 HEAD 和 `git status`。
- 本地分支只保留当前主线与 `codex/integration-staging`；远端分支不在本轮清理范围。
- 已验证外部 `branch-backup-20260905-v4.bundle`（68,024,704 字节）：包含完整历史、33 个 refs、HEAD=`a27a843`、`refs/stash` 和 `ORIG_HEAD`。它不包含本轮未提交文档卫生 diff；形成新提交后再按需刷新。三个历史 stash 不得删除。
- 文档卫生开始后观察到两个独立 owner 的未追踪计划：`2026-09-05-process-observer-stability-plan.md` 与 `2026-09-05-web-crud-authority-plan.md`。它们属于进行中工作，本任务不修改。
- 发布总判定见 [发布门禁审计](2026-09-04-release-gate-audit.md)：**BLOCKED / NOT RELEASE READY**。

## 已完成并合入

### 工程治理与环境

- 旧接力/一次性修复计划已从当前文档面移除；长期设计、架构、产品、release、migration 和 `docs/superpowers/**` 保留。
- Workspace 已完成当时已知的 Clippy 警告批次（`2d865da`），`rustfmt`/`clippy` 组件当前可用；后续源码提交仍必须重跑完整门禁。
- `nwflash-tauri` 测试宿主已嵌入 Common Controls v6 manifest（`e5102cc`），lib 和 integration tests 能在本机启动。保留报告：[Tauri 测试 manifest](2026-09-04-tauri-test-manifest-report.md)。

### Safe Flash

- 分区刷写失败决策功能已由 `604ade3` 合入：仅分区 `flash` 失败触发；用户可继续剩余分区或中止；取消不会误弹失败对话框；超时、会话失效和事件发送失败均 fail closed。
- 凭据、URL userinfo、私钥块等失败文本经过既有 trace 过滤；前端决策成功或提示失效后关闭旧弹窗。
- 当前流程基线保留在 [Safe Flash 流程与日志](2026-09-03-safe-flash-pipeline-and-logging.md)。

### 文件传输

- `82cc192`：文件页加入单次取消、busy 锁定和失败后解锁。
- `8f759ef`：upload/download 使用本次操作拥有的 partial、no-replace promote、独立 cleanup deadline；APK install 具有 deadline/cancel/终态测试。
- 初始证据与修复后证据分别保留在 [初始验证](2026-09-04-file-transfer-validation.md) 和 [P1 后验证](2026-09-04-file-transfer-post-p1-validation.md)。

### Cloudflare 与管理员后台

- 崩溃诊断文案已在 `b52cc70` 收窄为已实现的凭据/高风险内容过滤，没有虚构“所有哈希自动过滤”。
- 管理员用户 token 响应校验已由 `59fe97d` 合入。
- 审计导出完成协议和浏览器端校验已由 `741ef97`、`819cf25` 合入。
- API 版本头门禁已由 `e17bd81` 合入：七个受保护端点缺头/非法头分别返回 `400 CLIENT_VERSION_REQUIRED/INVALID`，低版本保持 `426`；健康、pin、遥测、崩溃、版本发现和客户端在线列表保留明确豁免。自动化证据为 Node `77/77`、Workerd `203/203`、Rust contract `28/28`、typecheck/dry-run 通过；未部署。

## 当前进行中

### 1. Process observer 稳定性

独立 owner 按 [稳定性计划](2026-09-05-process-observer-stability-plan.md) 处理 full suite 下 `observer_failure_reports_loss_without_stopping_pipe_drain` 的 loss 数量时序。目标是区分 callback failure 与真实 queue overflow，不用 sleep、ignore 或放宽生产上限掩盖问题。完成后重跑 `nwflash-windows`、workspace、clippy、check。

### 2. Web CRUD authority

独立 owner 按 [Web CRUD authority 计划](2026-09-05-web-crud-authority-plan.md) 处理管理员 app-version/API-user CRUD 的精确路由、ID、字段完整校验、原子 update、`meta.changes` 和未知目标 404。不得借此改变 schema、用户门户或部署配置。

### 3. 文档卫生

本任务重写本计划与根 `PROJECT_PROGRESS.md`、更新保留报告、删除完成计划/旧 handoff 并检查死引用。只允许文档改动，不提交或推送。

## 下一阶段

### A. 文件传输 P1-03

按 [P1-03 计划](2026-09-04-file-transfer-p1-03-plan.md)依次完成：

1. 由真实 builder 生成并执行 host shell 合同，覆盖普通目标、已存在目标、目录、symlink/broken symlink、quoting 和 cleanup。
2. 构建专用 `e2e` Tauri binary；native WDIO 必须真实经过 WebView → Tauri serde → coordinator → feature-gated executor，不能只用 JS direct mock。
3. 经设备所有者授权后，在专属 `/data/local/tmp` sandbox 验证 Android/toybox `sh -c`、`mv -n`、`rm -f --`、`[ -e ]/[ -L ]`；不支持时保持 fail closed。
4. 单独登记远端 destination-directory 竞态、Windows reparse TOCTOU、字节进度和 selectedRemote 等 P2，不在验证任务中顺手扩大生产行为。

### B. Web/API 剩余项

以 [Web/API 验证报告](2026-09-04-web-api-validation.md)为问题索引：

- 完成 CRUD authority 后，继续处理删除用户的 session/lease 清理、服务端 `force_exit` 展示、50-byte 查询边界、run/user ownership、时区口径、ROM failure reason 和首次管理员部署文档。
- API 版本门禁部署前确认旧无头客户端收到 400 的升级体验；`/api/online` 豁免依赖 heartbeat 是明确产品取舍。
- `/health`、`/api/me`、`/api/rom` 方法闭集与公共响应缓存属于独立卡，不混入已完成版本门禁。

### C. Plan C 日志链路

发布策略文档仍把生产 trace 接线视为未闭合。按依赖顺序重新审计并完成：

1. `TraceOutputSession` sealed attempts → metadata spool/uploader 实际适配。
2. 七类 operation 的 producer/observer 接线和 legacy observer 迁移。
3. producer→spool 崩溃矩阵、credential rejection re-seal 和七天 durable-loss。
4. 全部生产适配器上线并验证后，才退休 Tauri V1 `UsageLogReporter`。
5. 重启 payload replay 仍是独立产品决策；当前 metadata-only durable-loss 不能冒充 replay。

### D. 当前 HEAD 完整非部署门禁

冻结工作区后运行并记录完整结果：

```powershell
pwsh -NoLogo -NoProfile -NonInteractive -File scripts/Test-PowerShellRuntimeBoundary.ps1
cargo fmt --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --all -- --check
cargo check --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --workspace --all-targets
cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --workspace
cargo clippy --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --workspace --all-targets -- -D warnings
npm --prefix src/Nwflash.Desktop run test
npm --prefix src/Nwflash.Desktop run build
npm --prefix cloudflare test
npm --prefix cloudflare run test:workerd
npm --prefix cloudflare run test:admin
npm --prefix cloudflare run typecheck
npm --prefix cloudflare/user test
npm --prefix cloudflare/user run typecheck
git diff --check
```

安装 `src/Nwflash.Desktop/e2e-tests` 锁定依赖后，再运行 native/visual WDIO。测试缓存和 `.artifacts` 只能在确认进程结束、路径受控且 owner 同意后清理。

### E. 发布与外部验收

完整顺序与命令见 [发布门禁审计](2026-09-04-release-gate-audit.md)：

1. Git bundle 与固定 HEAD 快照。
2. 当前 HEAD 完整非部署门禁、production/no-bundle 与 native WDIO。
3. 固定 VMProtect SDK 校验、真实 release EXE/PDB/MAP 和六 marker link/layout。
4. 经授权执行 Lite GUI、compiler log/marker review、protected runtime/CRC/import removal。
5. EXE Authenticode 签名 → 以签名 EXE 构建 NSIS → installer 签名 → 安装/卸载与精确文件树验收。
6. 经授权执行驱动与真机矩阵。
7. 建立 D1 recovery point 后按 schema/API → UI 顺序部署 Cloudflare，并保留合成 smoke 与回滚证据。

## 不确定事项与停止条件

| 事项 | 当前处理 |
|---|---|
| Process observer 并行 flaky | 独立稳定性卡；未收口前 workspace test 非发布绿灯 |
| 文件传输 Android shell 能力 | host fixture 不能代替 toybox；没有设备授权则标未验证 |
| Native WDIO 依赖/专用 binary | 缺失即 blocked；不得放宽 binary allowlist 或用 direct mock 冒充 |
| API 旧客户端兼容 | 门禁已实现，部署前确认客户端覆盖和 400 升级体验 |
| Plan C 生产接线 | 以 release/VMP 文档的保守边界为准，重新审计后关闭 |
| VMProtect/签名/安装器 | 必须由受控 operator 和真实 evidence chain 完成 |
| 真机/驱动 | 只在已批准、可恢复的专用设备/主机执行 |
| 生产部署 | 需要 D1 recovery point、secret/domain 核验和明确授权 |

## 交付格式

每个子任务必须报告任务/owner、精确文件、计划与实际差异、测试命令及结果、未解决风险、提交 hash（如有）和可逆回退方式。总指挥只在报告、测试和 diff 一致后合入；源码存在、fixture 通过或旧产物存在都不等于发布完成。
