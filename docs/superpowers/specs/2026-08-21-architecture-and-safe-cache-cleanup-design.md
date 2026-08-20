# 项目架构与安全构建缓存清理设计

**状态：** 已由用户确认以“深度但保守”的范围持续执行。

## 目标

在不删除发布物、依赖或用户工作区内容的前提下，清理 NWflash Tauri 开发缓存与 E2E 日志，并将 `docs/project-architecture.md` 更新为以当前 React/Tauri/Rust 源码为事实源的规范项目架构。

## 不采用的方案

1. **清空整个 `src-tauri/target`**：不采用，因为它同时包含 `release/bundle`、`nsis`、`resources` 和已生成的 release 可执行文件。
2. **清理所有可再生成目录**（包括 `node_modules`、`dist`、`gen`、`.superpowers`）：不采用，这会丢失本地依赖、未追踪的前端产物或既有 agent 记录，且空间回收不如 Tauri debug 缓存显著。
3. **推荐：只清理可证明的 debug 缓存和 E2E 日志**。这个方案回收约 36.2 GiB 的 Tauri debug 缓存和约 1.5 MiB 日志，同时保留 release 输出。

## 清理范围与安全性

只处理以下目标：

| 目标 | 理由 | 执行方式 |
| --- | --- | --- |
| `src/Nwflash.Desktop/src-tauri/target/debug` | Cargo/Tauri `dev` profile 编译缓存，可从源码重建 | 先用 `cargo clean --dry-run --profile dev` 验证，再用相同 manifest/profile 清理 |
| `src/Nwflash.Desktop/e2e-tests/logs/*.log` | 历史 WDIO 运行日志，已被忽略规则覆盖 | 先列出绝对路径、验证均位于该目录，再定向删除 |

必须保留以下路径：

- `target/release-rebuild/`：当前包含 release `nwflash-desktop.exe`、调试符号和 resources，不能因“rebuild”命名被当作普通缓存删除。
- `src/Nwflash.Desktop/src-tauri/target/release/`，尤其 `bundle`、`nsis`、`resources` 和生成的 EXE。
- `node_modules/`、`dist/`、`src-tauri/gen/`、`.superpowers/`、`artifacts/`、资源包和所有其他已追踪/未追踪源文件。

清理前需解析每个绝对路径并验证它仍在工作区内。清理后需再次检查 debug 目录和日志文件均已消失，而 release 产物仍存在。不对仓库根、用户主目录或 `%TEMP%` 执行递归删除。

## 规范项目架构文档

`docs/project-architecture.md` 是唯一的项目级规范文档，保留 WPF 作为历史/视觉基线的边界，以 `src/Nwflash.Desktop/` 为当前客户端的事实源。更新后的结构为：

1. 系统范围和仓库责任；
2. React → Tauri → Rust Cargo crates 的依赖方向；
3. `AppState`、会话、设备快照、operation coordinator 和 runtime capability 的状态/事件模型；
4. IPC 公开能力、输入重新校验和敏感数据边界；
5. 设备、进程、Quick Flash、ROOT、Safe Flash 和固件处理的受控数据流；
6. 资源供应、staging 和完整性保证；
7. React 责任、Cloudflare 服务边界、测试/发布和外部验收状态。

文档必须修正 serial 模型：`DeviceSnapshot`/TypeScript 设备 DTO 包含 serial 供界面展示，但浏览器不能提交、选择或伪造执行 serial；Rust 从当前设备 runtime 派生实际命令目标。Quick Flash 和部分 ROOT 高风险流程会将预检/工件与当前 serial 进行等值复核，发现设备变更时拒绝继续。

为防止项目级概要与规范文档彼此矛盾，同步对以下高层摘要做最小文字修正：`docs/index.md`、`docs/architecture-tauri-migration.md` 和 `src/Nwflash.Desktop/docs/rust-tauri-architecture.md`。不重写历史计划、WPF 基线或发布 runbook。

## 验收

- Cargo dry-run 和清理后检查证明只是 Tauri `debug` 产物被移除，release 产物仍在。
- E2E 日志目录中没有遗留 `.log` 文件。
- 当前项目架构文档与 Cargo manifests、`AppState`、command 注册表、DTO 和 React event 边界一致。
- 上述四份文档的定向 diff 通过 `git diff --check`，且不声称真机、WDIO native、签名或安装器发布已完成。
