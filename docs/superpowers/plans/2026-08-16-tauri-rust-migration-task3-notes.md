# 2026-08-16 Task3 Notes（奶蛙Flash Tauri 壳体任务）

## 完成目标

- 已根据 `docs/superpowers/plans/2026-08-15-tauri-rust-migration.md` 的 Task3 约束补齐前端壳体结构文件。
- 建立 `AppShell` + `Sidebar` + `DeviceStatusPanel` + `OperationProgressPanel` + `ModalLayer` 组件链。
- 增加 `app/window-state.ts` 与 `app/ipc-events.ts`，为 Task3 的状态/事件模型提供最小接口。
- 拆分 `styles` 为 `tokens.css`、`reset.css`、`shell.css`、`components.css`，`app.css` 聚合这些设计令牌与公共规则。
- 补充窗口壳体测试：`components/AppShell.test.tsx`、`app/window-state.test.ts`。

## 目前架构（Task3 对齐）

- `app/pageManifest.ts`
  - 继续维护 11 页导航结构与中文文案映射。
- `app/window-state.ts`
  - 定义五类进度种类（`quick/lineFlash/safeFlash/firmwareExtract/device`）及固定优先级；
  - 提供 `resolveProgressText` 与空闲提示转换函数，供壳体进度显示使用。
- `app/ipc-events.ts`
  - 建立 operation、modal、session 的事件/payload 约定常量与类型。
- `components/AppShell.tsx`
  - 组合侧边栏、账号栏/时钟、统一进度区、窗口控制、模态层与页面内容插槽；
  - 统一进度文本与登出禁用策略由壳体层处理。
- `components/Sidebar.tsx`
  - 按导航分组与顺序渲染 11 个页面入口，并暴露 `data-page-id` 便于回归测试。
- `components/DeviceStatusPanel.tsx`
  - 显示当前账号与时钟。
- `components/OperationProgressPanel.tsx`
  - 显示统一进度文案（含“无进行中的操作”空闲态）。
- `styles/*.css`
  - 将视觉变量、重置、壳体布局、组件样式进行分层，避免内联重复样式。

## 对照项（当前阶段自检）

- 11 个导航项：通过 `NWFLASH_APP_PAGES` 保持不变。
- 统一进度优先级覆盖：`AppShell` 与 `window-state` 测试覆盖 `quick > lineFlash > safeFlash > firmwareExtract > device`。
- 登出禁用态：`operations` 有值时 `AppShell` 内的“退出”按钮禁用。
- 无组件库样式：仅保留自研样式 token 与原生 DOM 组件。

## 下一步

- 继续执行 Task3 Step4：启动 `npm --prefix src/Nwflash.Desktop run test -- AppShell window-state` 确认通过；确认启动截图比对项。
- Step5 之前，暂不引入 Task4 的领域模型行为，仅保持壳体可渲染即可。

## 2026-08-16 追加进展（会话联动）

- 已将 `App` 登录态从演示态替换为 Rust 会话命令联动：
  - 登录成功：`auth_login` -> `session_start`
  - 会话校验：`session_state` + `auth_validate_token`
  - 登出：`session_stop` + `auth_logout`
  - 关闭/更新事件：`session:force-exit`、`session:update-required` 推送到前端并回退到未登录 UI
- `OperationLogPanel` 已补充返回值防御，防止后端非数组响应导致前端渲染崩溃。
- 当前本地验收：
  - `npm --prefix src/Nwflash.Desktop run test -- App AppSessionLifecycle AppSessionAuthFlow AppShell window-state`
  - `cargo test -p nwflash-infrastructure --test api_contract --test auth_contract --test version_contract`
  - `cargo test -p nwflash-application --test operation_coordinator --test session_lifecycle`

## 2026-08-16 追加进展（会话健壮性）

- 修正会话恢复路径异常处理：`App.tsx` 在 `session_state` 或 `auth_validate_token` 失败/抛错时，统一回退未登录态（避免卡死在半登录状态）。
- 新增 `AppSessionAuthFlow.test.tsx` 回归：`auth_validate_token` 失败会显示登录界面。

## 2026-08-16 追加进展（版本门禁 426 映射）

- 修复 `nwflash_infrastructure::VersionClient::check`：
  - 不再把 `/api/app/version` 返回的 `UPDATE_REQUIRED` 直接当作“放行”。
  - 426 分支现在映射为 `update_required: true` 与 `force_update: true`，并透传 `latest/min_version/download_url`，供前端版本门禁展示。
- 新增 `version_contract` 测试用例覆盖 `check()` 在 426 响应下返回更新门禁（`update_required = true`、`force_update = true`）。

## 2026-08-16 追加进展（启动版本门禁）

- 已在 `src/app/App.tsx` 增加启动 `version_check` 门控：启动恢复会话前先执行 `/api/app/version`；
- 结果为 `update_required=true` 或 `force_update=true` 时，前端展示更新提示、清空会话与登录态并设置禁用登录；
- `session:update-required` 事件与启动时版本检查共享同一“阻断登录”状态。
- 新增 `AppSessionAuthFlow.test.tsx` 回归：版本检查命中更新要求时，登录按钮保持禁用且未触发 `auth_login` 调用。

## 2026-08-16 追加进展（操作日志页防御与回归）

- `OperationLogPage.tsx` 持续采用防御式返回值解析，移除多余类型断言路径后直接落库标准日志列表。
- 新增 `OperationLogPage.test.tsx` 回归覆盖：正常两条日志、空列表、非法结构回退空态、`operation_logs_snapshot` 异常提示。
- 最小化测试建议执行：`npm --prefix src/Nwflash.Desktop run test -- OperationLogPage`

## 2026-08-16 追加进展（任务预检快照闭环）

- 新增 `AppSessionLifecycle.test.tsx` 覆盖：`operation:snapshot` 事件会更新右上统一进度、任务时禁用登出、空闲时恢复可登出。
- 与 `QuickFlashPage`/`FileManagerPage`/`OperationLogPage` 一并执行前端最小闭环，确认命令预检路径与事件快照路由联动通过。
- 结论：`npm --prefix src/Nwflash.Desktop run test -- AppSessionLifecycle.test.tsx AppSessionAuthFlow.test.tsx AppShell.test.tsx OperationLogPage.test.tsx QuickFlashPage.test.tsx FileManagerPage.test.tsx` 全通过。

## 2026-08-16 追加进展（Task4 接口面联调保留）

- 重新校验会话/刷写接口面与会话桥接关键测试：
  - `cargo test --test api_contract --test auth_contract --test version_contract`
  - `cargo test -p nwflash-tauri --lib`
  - `cargo test -p nwflash-application --test quick_flash --test file_transfer`
  - `cargo test -p nwflash-application --test operation_coordinator --test session_lifecycle`
- 结果全部通过：API 契约、命令 DTO、传输/预检、操作门禁与会话生命周期测试链路闭环。
- 补充执行：`cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml`
  - 结果通过：`build_smoke` 与现有单元目标均绿灯（当前 Workspace Test 仅覆盖已完成任务段）。
