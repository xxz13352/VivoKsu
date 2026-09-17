# NWFlash 当前项目进展

更新时间：2026-09-05（Asia/Shanghai）

本文只提供当前状态入口，不保存逐会话流水。详细工作顺序见 [接力迭代总计划](docs/2026-09-04-iteration-plan.md)，发布判定见 [发布门禁审计](docs/2026-09-04-release-gate-audit.md)。

## 当前结论

- 活跃主线：`codex/vmp-release-completion`。
- 文档卫生任务开始时的提交：`a27a843 chore(workspace): finalize relay plans and cleanup`；执行任何门禁前必须重新记录实际 HEAD。
- 本地仅保留主线和 `codex/integration-staging` 两个分支。
- 功能源码已包含 Safe Flash 分区失败决策、文件传输取消与原子收尾、Tauri 测试 manifest、管理员 token/导出修复，以及公共 API 版本头 fail-closed 门禁。
- 当前状态仍是 **NOT RELEASE READY**：完整发布证据链、native WDIO、受保护构建、签名、安装器、真机和生产部署尚未闭合。
- 不使用工程完成百分比；“源码已合入”“定向测试通过”不能替代发布验收。

## 已合入的当前成果

| 提交 | 成果 |
|---|---|
| `2d865da` | 收口当时已知的 workspace Clippy 警告 |
| `82cc192` | 文件管理页加入可取消操作和稳定 busy 状态 |
| `e5102cc` | 为 `nwflash-tauri` 测试宿主嵌入 Common Controls v6 manifest，解除 `0xc0000139` |
| `604ade3` | Safe Flash 分区写失败时暂停并让用户继续或中止 |
| `59fe97d` | 管理后台严格校验创建/轮换用户的一次性 token 响应 |
| `741ef97`、`819cf25` | 管理后台采用可验证完成协议并校验流式审计导出 |
| `8f759ef` | 文件上传/下载使用自有临时目标、no-replace promote 和失败清理 |
| `e17bd81` | 七个受保护 API 路由强制合法 `X-Nwflash-Version`，保留明确豁免端点 |
| `f6b7dd7` | 保存文件传输 P1 修复后的分层验证报告 |
| `a27a843` | 删除旧 launch 配置并补齐当前发布/文件传输计划入口 |

## 当前可信的自动化证据

- API 版本门禁：Node `77/77`、Workerd `203/203`、Rust API/version contract `28/28`，strict TypeScript 与 API/Web dry-run 通过；未部署。
- Tauri manifest：`nwflash-tauri` lib `300/300`、`mirror_runtime 1/1`、`release_probe 4/4`；测试 EXE 的 RT_MANIFEST 含 Common Controls 6.0。正式 release EXE 仍需重新生成并验收。
- 文件传输 P1 后验证：FileManager UI `16/16`，连同 PageFactory `19/19`；Tauri files `19/19`；application file manager `8/8`、file transfer `4/4`；coordinator `31/31`；file-ops `7/7`。完整 workspace 曾命中 process observer 时序 flaky，尚需收口并重跑。
- Cloudflare 管理后台在修复前的完整基线为 admin unit `135`、admin Workerd `51`、Chromium `31`、API Node `77`、Workerd `192`；F-01/F-02 修复已合入，但必须在当前 HEAD 重跑全套，不能沿用旧数字宣称发布通过。
- Safe Flash 有定向 application/UI/类型检查证据，功能已合入；当前 HEAD 的 workspace、Tauri、UI 和 production build 仍需统一复验。

## 未完成工作

### 源码与测试

- 执行 [文件传输 P1-03 计划](docs/2026-09-04-file-transfer-p1-03-plan.md)：host shell 合同、真实 WebView → Tauri native E2E，以及经授权的 Android/toybox 矩阵。
- 按 [Process observer 稳定性计划](docs/2026-09-05-process-observer-stability-plan.md) 收口 `observer_failure_reports_loss_without_stopping_pipe_drain` 的并发时序不稳定。
- 按 [Web CRUD authority 计划](docs/2026-09-05-web-crud-authority-plan.md) 完成精确路由、正整数 ID、无效 mutation、`meta.changes` 和未知目标 404。
- 继续处理 [Web/API 验证报告](docs/2026-09-04-web-api-validation.md) 中尚未关闭的会话清理、force-exit、查询字节边界、详情 ownership、时区、ROM 原因和首次部署文档问题。
- API 版本门禁已经实现；发布前仍须确认旧版无头客户端收到 `400` 的升级体验、`/api/online` 豁免依赖 heartbeat 的产品取舍，以及独立的方法闭集/缓存问题。
- Plan C 生产追踪仍按发布策略文档视为未闭合：实际 producer/observer、sealed attempt → durable spool 接线、崩溃/re-seal/七天 durable-loss 矩阵和 V1 reporter 退休顺序需重新审计后完成。

### 发布与外部环境

- 已验证 `C:\Users\17254\Desktop\VivoKsu-quarantine\branch-backup-20260905-v4.bundle`：68,024,704 字节、完整历史、33 个 refs，HEAD=`a27a843`，并包含 `refs/stash` 与 `ORIG_HEAD`。它不包含本轮未提交文档卫生 diff；形成新提交后再按需刷新。
- 在固定 HEAD 上运行完整 Rust fmt/check/test/clippy、桌面 UI/build、Cloudflare API/admin/user 和 native WDIO 门禁。
- 使用固定 VMProtect SDK 完成真实 release link、PDB/MAP、六个 marker 与八个 SDK import 检查。
- 经授权执行 VMProtect Lite GUI、compiler log/marker review、protected runtime/CRC 和 import removal。
- 用授权证书签名 EXE，基于签名 EXE重新构建并签名 NSIS；完成安装/卸载和精确文件树验证。
- 经设备所有者批准执行驱动与真机矩阵；mock 不能替代这些结果。
- 取得 D1 recovery point、生产 secret 和运维授权后，才可执行 Cloudflare migration、部署和部署后 smoke。

## 当前文档入口

- [接力迭代总计划](docs/2026-09-04-iteration-plan.md)
- [发布门禁审计](docs/2026-09-04-release-gate-audit.md)
- [文件传输初始验证](docs/2026-09-04-file-transfer-validation.md)
- [文件传输 P1 后验证](docs/2026-09-04-file-transfer-post-p1-validation.md)
- [文件传输 P1-03 计划](docs/2026-09-04-file-transfer-p1-03-plan.md)
- [管理员 Web/API 验证](docs/2026-09-04-web-api-validation.md)
- [Tauri 测试 manifest 报告](docs/2026-09-04-tauri-test-manifest-report.md)
- [Safe Flash 流程与日志](docs/2026-09-03-safe-flash-pipeline-and-logging.md)
- [当前项目架构](docs/project-architecture.md)
- [产品决策](docs/product-decisions.md)

## 操作边界

- 未经单独授权，不推送、部署、签名、运行 VMProtect、安装驱动或连接真机。
- 不用 `git reset --hard`、`git clean`、广泛 worktree prune、reflog expire 或对象 prune 处理工作区。
- 发布候选必须绑定一个固定提交、完整测试记录和同一条 evidence chain；不得复用旧 target 中的 EXE、installer 或 sidecar。
