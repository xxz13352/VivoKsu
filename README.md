# 奶蛙Flash（NWflash）

> 当前桌面端为 `src/Nwflash.Desktop/` 中的 React + Tauri + Rust 客户端。Windows 发布物使用 `scripts/Verify-TauriRelease.ps1` 验证每用户 NSIS 布局、内置资源与 SHA-256 清单。

奶蛙Flash是面向 Vivo 设备的 Windows 刷机与 Root 工具。客户端使用账号登录，后端由 Cloudflare Workers 与 D1 承载认证、版本策略、操作授权、在线状态和固件服务。

## 当前版本

- 活跃客户端：`src/Nwflash.Desktop/`（React + Tauri + Rust）。
- 活跃发布资源：`src/Nwflash.Desktop/src-tauri/resources/`。
- 活跃发布脚本：`scripts/Publish-TauriRelease.ps1`、`scripts/Verify-TauriRelease.ps1` 和 `scripts/Test-TauriRelease.ps1`。
- C# / WPF 客户端已封存至 [archive/csharp/README.md](archive/csharp/README.md)，不参与当前构建、发布或后续版本更新。

## 功能

- ADB / Fastboot 设备发现、设备信息和驱动安装。
- 快速刷写、分区工作台、VIVO 线刷和双槽处理。
- 本地与在线固件检查、payload 提取、ROOT 与镜像工件流程。
- scrcpy 投屏、设备文件管理、统一操作日志与进度。
- Cloudflare 会话、版本门禁、操作授权和在线状态。

## 代码结构

```text
VivoKsu 工具/
├─ src/Nwflash.Desktop/                 # 活跃 React + Tauri + Rust 客户端
│  ├─ src/                              # React 页面、组件、状态与 IPC DTO
│  ├─ src/assets/                       # 前端自有品牌资源
│  ├─ src-tauri/crates/                 # Rust domain/application/infrastructure/windows/tauri
│  └─ src-tauri/resources/              # 唯一活跃发布资源
├─ cloudflare/                           # Workers、D1、后台、用户门户和官网
├─ packaging/release/                   # Tauri 发布资源清单
├─ scripts/                              # 活跃 Tauri 构建、签名、发布和验证脚本
├─ docs/                                 # 活跃产品、架构和验收文档
└─ archive/csharp/                      # 已冻结的 C# / WPF 历史版本
```

## 开发与验证

在仓库根目录运行：

```powershell
# React 单元/组件测试与生产前端构建
npm run test --prefix src/Nwflash.Desktop
npm run build --prefix src/Nwflash.Desktop

# Rust workspace 测试
cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --workspace

# Tauri 二进制构建（不打安装包）
npm run tauri --prefix src/Nwflash.Desktop -- build --no-bundle

# 发布契约与安装器验证
powershell -ExecutionPolicy Bypass -File scripts/Test-TauriRelease.ps1
powershell -ExecutionPolicy Bypass -File scripts/Verify-TauriRelease.ps1 -ReleaseRoot artifacts/tauri-release
```

## 发布

正式发布由 `scripts/Publish-TauriRelease.ps1` 生成。它以
`packaging/release/tauri-resources.json` 的精确 allowlist 从 Tauri 自有资源目录取文件、校验 SHA-256，并生成每用户 NSIS 安装器。发布后使用 `scripts/Verify-TauriRelease.ps1` 验证 release root；该验证器只读取和校验已生成的清单，不覆盖清单。

ROOT 管理器 APK、payload_dumper、platform-tools、驱动、root-tools 和 scrcpy 的发布输入均在 `src/Nwflash.Desktop/src-tauri/resources/`，不依赖 C# 目录。

## 文档

- [当前项目架构](docs/project-architecture.md)
- [Rust/Tauri 客户端架构](src/Nwflash.Desktop/docs/rust-tauri-architecture.md)
- [API 契约](cloudflare/API.md)
- [文档索引](docs/index.md)
- [C# / WPF 归档说明](archive/csharp/README.md)
