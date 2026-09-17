# 奶蛙Flash Tauri Desktop

## 目标

- 桌面端位于 `src/Nwflash.Desktop/`，使用 Tauri + React + Rust。
- 可见产品名固定为 **奶蛙Flash**；代码名和 API 版本头继续使用 NWflash。
- 浏览器仅提交封闭 DTO；文件管理 DTO 可以携带当前列表中的受限设备路径和原生文件对话框返回的本地路径，Rust 会重新校验。浏览器不提交设备 serial，也不获得 token、任意 shell、任意程序/参数或任意 HTTP 能力。
- 设备程序必须由 Rust 生成程序名和参数数组，并在 `OperationCoordinator` 的互斥、取消与失败收尾范围内运行。

## 架构文档

- [Rust/Tauri 客户端架构](docs/rust-tauri-architecture.md)：workspace 分层、IPC 能力边界、单设备模型、资源完整性、构建命令与未完成项。
- [项目级架构](../../docs/project-architecture.md)：当前发布边界、资源所有权、测试与验收规范。

## 测试与故障注入

- 真机准入、安全分级与记录模板：`docs/migration-baselines/device-acceptance-matrix.md`
- Rust E2E 故障注入：`src-tauri/tests/e2e/{api,operation,device_process,firmware}.rs`
- 所有 Cloudflare、GitHub、下载服务器、adb、fastboot 和 payload_dumper 故障测试使用本机 mock、录制输出或注入执行器；scrcpy 使用无效临时 EXE 覆盖真实进程启动失败。全部测试都不需要真实 token 或设备。

## 精确验证命令

在仓库根目录使用 PowerShell 运行：

```powershell
# Rust workspace（含故障注入）
cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --workspace

# React 单元/组件测试
npm run test --prefix src/Nwflash.Desktop

# 生产前端构建
npm run build --prefix src/Nwflash.Desktop

# 生产 Tauri 二进制构建（不打安装包）
npm run tauri --prefix src/Nwflash.Desktop -- build --no-bundle

# WDIO 原生 Tauri 交互 + 独立生产前端视觉门禁
npm run test --prefix src/Nwflash.Desktop/e2e-tests
```

## Windows release

正式发布从仓库根目录使用 PowerShell 7.4+ 执行 `scripts/Publish-TauriRelease.ps1` 的 `PrepareManual`/`FinalizeManual` 两阶段流程；自动 VMProtect console 入口被禁用，Lite GUI 输出、compiler log、marker review 和 `accepted.json` 必须先形成完整证据链。Finalize 阶段再签名 EXE、重建并签名每用户 NSIS、验证安装/卸载和最终清单。`-DevelopmentUnsigned` 仅供非发布暂存。所有 platform-tools、Vivo 驱动、root-tools、完整 scrcpy、管理器 APK 和 payload_dumper 都来自 Tauri 资源树的精确发布 allowlist。当前状态与命令见 [发布门禁审计](../../docs/2026-09-04-release-gate-audit.md) 和 [VMP/签名运行手册](../../docs/release/tauri-vmp-signing-runbook.md)。

WDIO 的交互套件会先以 `VITE_NWFLASH_WDIO_E2E=true` 构建专用 IPC bridge，再启动带 `e2e` feature 的 Tauri 二进制。视觉套件会重新执行普通 `npm run build`，验证生产前端不包含测试 bridge。嵌入式 provider 在会话结束后可能输出 `Failed to clear mock store: A sessionId is required`；只有在全部规格通过且进程退出码为 0 时才能视为通过。

## C# 历史版本

C# / WPF 客户端已封存到 [../../archive/csharp/README.md](../../archive/csharp/README.md)。它不参与 Rust/Tauri 的构建、测试、发布或资源供应；如需历史验证，请从归档目录单独运行其 .NET 解决方案。

## 设备验收约束

- 默认只运行无设备的 mock/录制测试。
- 只读 ADB/Fastboot 检测也必须取得设备所有者批准并确认当前唯一设备已连接；工具不做跨步骤或跨启动的 serial binding。
- 文件列表、下载、上传、安装和删除只允许使用无敏感数据的专用测试设备；浏览器会承载受限路径 DTO，但 serial 和最终程序/参数始终由 Rust 生成。
- 驱动安装只在可回滚测试主机或 VM 运行。
- Quick Flash、分区擦写、VIVO 线刷和 ROOT 只允许在专用、已备份、已演练恢复路径的设备上运行，并按验收矩阵留档。
- 未满足准入条件时不得用“仅验证一下”为由连接日用机执行破坏性操作。

`cloudflare/**` 是独立后端契约目录，桌面迁移测试不得修改该目录。
