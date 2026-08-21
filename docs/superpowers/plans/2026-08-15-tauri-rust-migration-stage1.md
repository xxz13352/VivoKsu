# 2026-08-15 NWflash Rust 迁移执行阶段：Tauri 受限脚手架（Stage 1）

## 完成情况

- 建立 `src/Nwflash.Desktop/` 独立桌面工作区（React + Vite + TypeScript + Rust）。
- 建立 Rust 工作区元信息与 5 个目标 crate 骨架：
  - `crates/nwflash-domain`
  - `crates/nwflash-application`
  - `crates/nwflash-infrastructure`
  - `crates/nwflash-windows`
  - `crates/nwflash-tauri`
- 建立前端入口（`index.html`, `src/main.tsx`, `src/App.tsx`）和最小品牌约束测试。
- 建立 `src-tauri/tests/build_smoke.rs` 作为阶段性验收测试。
- 新增 `src-tauri/tauri.conf.json` 与 `src-tauri/capabilities/default.json`（最小权限声明）。
- 生成前端依赖锁文件 `src/Nwflash.Desktop/package-lock.json`。

## 关键架构边界（当前阶段）

- `domain`：纯 Rust 领域定义（不依赖操作系统 / 网络 / UI）。
- `application`：行为编排占位层（当前先保留最小类型与入口）。
- `infrastructure`：外部系统适配层占位（待后续填充）。
- `windows`：平台能力适配占位（ADB/Fastboot 进程控制在后续阶段补齐）。
- `nwflash-tauri`：Tauri 命令桥占位（当前仍以可运行二进制入口为先）。
- `src`（frontend）：前端只负责 brand 呈现与状态渲染，不接触凭据、命令拼接、进程控制。

## 约束确认

- 客户端展示名保持为 **奶蛙Flash**，技术名使用 **NWflash/NWF** 约定。
- 未改动 `cloudflare/**`，不接触外部服务端实现。
- 本阶段不引入任意命令字符串拼接、凭据持久化、HTTP 调用，作为下一阶段前置条件。

## 验收命令与结果

- `cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --test build_smoke` ✅ 通过（2/2）。
- `npm --prefix src/Nwflash.Desktop run test` ✅ 通过（1/1）。
- `npm --prefix src/Nwflash.Desktop run build` ✅ 通过（Vite build 成功）。
