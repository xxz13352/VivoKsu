# C# 封存与 Rust 独立化设计

## 目标

将 WPF/C# 桌面端完整保留为可恢复的历史版本，同时让 `src/Nwflash.Desktop/` 的 React + Tauri + Rust 客户端不再依赖任何活跃 C# 源码、测试、资源或发布输入，并作为唯一继续更新的桌面端版本。

## 封存边界

以下内容移动到 `archive/csharp/`，保留原有相对目录结构，以便归档版本仍能独立使用其解决方案和项目引用：

- 根 `VivoKsu.slnx`。
- `src/VivoKsu.App/` 和 `src/VivoKsu.Bootstrapper/`。
- `tests/VivoKsu.App.Tests/` 和 `tests/VivoKsu.VisualCapture/`。
- 仅用于 WPF 发布、窗口自动化和旧线刷验证的 PowerShell 脚本。
- 明确描述 WPF 实现或 WPF→Tauri 迁移基线的历史架构、线刷和测试映射文档。

归档根目录新增说明，注明 C# 不参与当前构建、发布或版本迭代，并给出从 `archive/csharp/` 运行其解决方案和测试的命令。

## Rust 自有资源与发布

Rust/Tauri 的 `src/Nwflash.Desktop/src-tauri/resources/` 已包含当前发布所需的 platform-tools、驱动、ROOT 工具、APK、payload_dumper 和 scrcpy，且与此前 C# 输入文件哈希相同。该目录成为唯一活跃发布资源源：

- 发布资源清单改为从它读取，不再引用 `src/VivoKsu.App/`。
- 上传外置资源的脚本改为从它读取。
- 品牌 Logo 复制到 React 自有 `src/Nwflash.Desktop/src/assets/`，两个组件改为本地导入。
- C# 独有、Rust 当前未使用的 `Sukisu.APK` 随 C# 版本封存，不进入活跃 Rust 资源目录。

## 解除迁移耦合

当前 Rust 端有一个测试会遍历 `tests/VivoKsu.App.Tests` 并验证迁移映射。该测试和映射只服务迁移完成前的覆盖率追溯，因此移入封存历史：

- 删除 Rust 测试对 C# 测试目录和映射文档的读取与断言。
- 驱动归档测试改为直接使用 Tauri 自有 `resources/drivers/vivo-usb-driver.7z`。
- 当前 Rust 测试只验证 Rust/Tauri 的功能、资源和发布契约。

## 活跃文档与验证

根 README、文档索引、项目架构和 Tauri README 仅描述 Rust/Tauri 作为当前客户端，并将 C# 标记为归档位置。实现完成后必须验证：

1. 活跃前端、Rust、脚本和发布清单不再引用活动 C# 路径。
2. `cargo test --workspace`、React 测试与生产构建通过。
3. Tauri 发布资源清单可从 Rust 自有资源树完整校验。
4. 归档 C# 解决方案仍可在 `archive/csharp/` 下构建和运行测试。

## 非目标

- 不修改 Cloudflare 后端契约。
- 不改动产品功能、Tauri IPC 或发布资源的内容与哈希。
- 不删除 C# 历史版本；封存内容由 Git 记录并可恢复。
