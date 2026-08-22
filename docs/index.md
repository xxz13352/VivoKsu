# NWflash 文档索引

> 当前桌面端唯一主线是 `src/Nwflash.Desktop/` 的 React + Tauri + Rust 客户端。C# / WPF 历史版本已封存，不参与当前开发和发布。

## 当前文档

| 文档 | 用途 |
| --- | --- |
| [项目架构](project-architecture.md) | 当前客户端、Cloudflare 边界、资源、测试和发布规范 |
| [Rust/Tauri 客户端架构](../src/Nwflash.Desktop/docs/rust-tauri-architecture.md) | workspace 分层、IPC 和资源运行时细节 |
| [产品决策](product-decisions.md) | 当前产品约束与安全边界 |
| [设备验收矩阵](migration-baselines/device-acceptance-matrix.md) | 真机刷写、ROOT、驱动与发布验收要求 |
| [API 契约](../cloudflare/API.md) | 桌面端与 Cloudflare API 的公开契约 |
| [Cloudflare 部署](../cloudflare/README.md) | Worker、D1 和环境变量配置 |
| [C# / WPF 归档](../archive/csharp/README.md) | 冻结版本的位置、恢复和单独验证命令 |

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
powershell -ExecutionPolicy Bypass -File scripts/Test-TauriRelease.ps1
powershell -ExecutionPolicy Bypass -File scripts/Verify-TauriRelease.ps1 -ReleaseRoot artifacts/tauri-release
```

## 维护规则

- 新桌面功能、测试、资源和发布改动只进入 `src/Nwflash.Desktop/`、`packaging/` 与活跃 `scripts/`。
- `archive/csharp/` 仅保存历史版本；若必须查验或恢复，请在归档目录中单独构建，不要把其资源重新作为 Rust 发布输入。
- `cloudflare/**` 是独立后端契约目录，桌面端迁移或封存不应改变其公开 API。
