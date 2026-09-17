# NWflash Tauri Migration Stage2（2026-08-16）：Rust entrypoint 与 workspace build 的最小落地

## 一、阶段范围

本阶段沿用 `docs/superpowers/plans/2026-08-15-tauri-rust-migration.md` 与 `Task 2/3` 的边界，目标是将 Tauri 最小运行壳从“离线结构体”推进为“可成功编译 + 可打包”的约束骨架，不引入业务逻辑。

## 二、架构边界（当前实现）

- `src/Nwflash.Desktop/src-tauri/Cargo.toml`
  - 工作区成员：`nwflash-domain / nwflash-application / nwflash-infrastructure / nwflash-windows / nwflash-tauri`
  - 顶层仅承担宿主 crate 与版本属性约束
- `src-tauri/crates/nwflash-tauri/src/lib.rs`
  - 保留 `run_app(context)` 统一入口，集中设置窗口标题与事件注册桩
  - 仅保留 `invoke_handler([])` 与 `run(context)`，不承载设备/网络逻辑
- `src-tauri/src/main.rs`
  - 由主入口创建 `tauri::generate_context!()` 并传递给 `run_app`
- `src-tauri/tauri.conf.json`
  - 维持安全最小化 CSP，窗口参数与 `奶蛙Flash` 品牌名
- `src-tauri/icons/icon.png`
  - 补齐 Tauri 打包上下文对图标文件的硬依赖，避免构建阶段 abort

## 三、架构约束（保持）

- 可见品牌固定为 **奶蛙Flash**，技术名沿用 NWflash 约定
- 暂未落地任何 `Cloudflare`、ADB/Fastboot、进度/权限决策逻辑
- 前端 `src/Nwflash.Desktop/src` 仍为展示壳，仅保留页面导航和文案
- `docs/architecture.md` 未变更：仅用于历史 WPF 全量系统说明，Tauri 工程采用分阶段迁移方式补齐

## 四、本阶段验收

1. `cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --test build_smoke`
2. `npm --prefix src/Nwflash.Desktop run test`
3. `npm --prefix src/Nwflash.Desktop run build`
4. `npm --prefix src/Nwflash.Desktop run tauri build --no-bundle`

## 五、验证结果（2026-08-16）

- 1/2: `build_smoke` 通过（2/2）
- 2/2: 前端测试通过（2 个文件，3 条用例）
- 3/2: Vite 构建通过
- 4/2: Tauri release build 成功，输出位于 `src-tauri/target/release/nwflash-desktop.exe`

## 六、下一步

进入下一迭代前提：

- 建立前端壳体的交互与进度面板可用性测试（Task 3
  `AppShell/window-state`）
- 进一步引入 `crate::application` 的命令桥接口桩并为其编写失败/成功用例
- 在不新增持久化与外部执行权限的前提下，引入 Cloudflare 会话门禁命令
