# NWflash 文档索引

> 当前桌面端唯一主线是 `src/Nwflash.Desktop/` 的 React + Tauri + Rust 客户端。C# / WPF 历史版本已封存，不参与当前开发和发布。

## 当前文档

| 文档 | 用途 |
| --- | --- |
| [项目进展](../PROJECT_PROGRESS.md) | 简洁的当前状态、已合入成果、未完成项和主要证据 |
| [接力迭代总计划](2026-09-04-iteration-plan.md) | 当前执行顺序、进行中 owner、停止条件和发布前门禁 |
| [发布门禁审计](2026-09-04-release-gate-audit.md) | 当前 release readiness、外部授权边界和最终 evidence chain |
| [客户端纵深加固（P0–P3）](2026-09-21-client-defense-in-depth.md) | IPC 边界、两段式反调试、固件包验签、FFI lint 门禁的实施记录与取舍理由 |
| [项目架构](project-architecture.md) | 当前客户端、Cloudflare 边界、资源、测试和发布规范 |
| [Rust/Tauri 客户端架构](../src/Nwflash.Desktop/docs/rust-tauri-architecture.md) | workspace 分层、IPC 和资源运行时细节 |
| [产品决策](product-decisions.md) | 当前产品约束与安全边界 |
| [设备验收矩阵](migration-baselines/device-acceptance-matrix.md) | 真机刷写、ROOT、驱动与发布验收要求 |
| [VMP/签名运行手册](release/tauri-vmp-signing-runbook.md) | 受控构建、人工保护、签名、NSIS 和切换顺序 |
| [API 契约](../cloudflare/API.md) | 桌面端与 Cloudflare API 的公开契约 |
| [Cloudflare 部署](../cloudflare/README.md) | Worker、D1 和环境变量配置 |
| [C# / WPF 归档](../archive/csharp/README.md) | 冻结版本的位置、恢复和单独验证命令 |

## 当前验证与计划

| 文档 | 状态 |
| --- | --- |
| [文件传输初始验证](2026-09-04-file-transfer-validation.md) | P1 修复前的历史基线，保留作追溯 |
| [文件传输 P1 后验证](2026-09-04-file-transfer-post-p1-validation.md) | 当前 mock/定向证据与剩余风险 |
| [文件传输 P1-03](2026-09-04-file-transfer-p1-03-plan.md) | 待实现的 shell 合同、native WDIO 和设备矩阵 |
| [管理员 Web/API 验证](2026-09-04-web-api-validation.md) | F-01/F-02/API 版本门禁已关闭，其余发现继续跟踪 |
| [Process observer 稳定性](2026-09-05-process-observer-stability-plan.md) | 当前并发 loss 时序收口计划 |
| [Web CRUD authority](2026-09-05-web-crud-authority-plan.md) | 当前管理员 CRUD 后端 authority 计划 |
| [Tauri 测试 manifest](2026-09-04-tauri-test-manifest-report.md) | `0xc0000139` 修复及测试宿主证据 |
| [Safe Flash 流程与日志](2026-09-03-safe-flash-pipeline-and-logging.md) | 线刷流程基线；分区失败决策以当前源码为准 |
| [固件 Range 下载断流修复](2026-09-17-firmware-range-download-fix.md) | 线上下载 + 云提取的 Range 断流续传与超时收口；关闭 A33/A34 超时部分 |

## 活跃代码地图

```text
VivoKsu 工具/
├─ src/Nwflash.Desktop/                 # React + Tauri + Rust 客户端
│  ├─ src/                              # React 页面、组件、状态和 IPC DTO
│  ├─ src/assets/                       # 前端品牌资源
│  ├─ src-tauri/crates/                 # Rust domain/application/infrastructure/windows/tauri
│  └─ src-tauri/resources/              # 发布资源与完整性输入
├─ cloudflare/                           # API、后台、用户门户和官网
├─ packaging/release/                   # 发布资源 allowlist
├─ scripts/                              # 发布、签名与验证脚本
├─ docs/                                 # 活跃架构、决策和验收文档
└─ archive/csharp/                      # 封存的 C# / WPF 版本
```

## 常用命令

```powershell
npm run test --prefix src/Nwflash.Desktop
npm run build --prefix src/Nwflash.Desktop
cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --workspace
pwsh -NoLogo -NoProfile -NonInteractive -File scripts/Test-TauriRelease.ps1
pwsh -NoLogo -NoProfile -NonInteractive -File scripts/Verify-TauriRelease.ps1 -ReleaseRoot artifacts/tauri-release
```

## 维护规则

- 新桌面功能、测试、资源和发布改动只进入 `src/Nwflash.Desktop/`、`packaging/` 与活跃 `scripts/`。
- `archive/csharp/` 仅保存历史版本；若必须查验或恢复，请在归档目录中单独构建，不要把其资源重新作为 Rust 发布输入。
- `cloudflare/**` 是独立后端契约目录，桌面端迁移或封存不应改变其公开 API。
- 一次性计划和会话 handoff 完成后，应先把仍有效的风险迁入当前迭代/发布入口，再删除旧文件并检查死链接。
