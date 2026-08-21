# 会话撤销与即时设备目标修复 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 修复登录切换、固件工件会话隔离和 Quick Flash 的授权后即时设备发现。

**Architecture:** 登录切换复用现有 idle teardown；固件工件在同一 teardown barrier 清除 owned staging；Quick Flash 在 `run_async` 内运行实时 transport discovery 后重建命令。所有方案避免运行时摘要和跨步骤 serial 绑定。

**Tech Stack:** Rust 2021、Tauri 2、Tokio、Cargo workspace。

## Global Constraints

- 不新增运行时 SHA-256、hash、fingerprint、checksum 或内容比较。
- 不新增跨步骤 phone serial 比较、serial-bound capability 或 serial 变化拒绝。
- serial 只作当前实时命令的 ADB/Fastboot 目标；多设备拒绝和 fastbootd `is-userspace` 检查保留。
- 任何 capability 撤销前必须持有 `OperationCoordinator::try_acquire_idle()` 成功得到的 idle lease。
- Rust-owned staging 只能在 runtime 锁/epoch barrier 释放后递归删除；外部镜像路径绝不删除。
- 先写 RED 回归测试并确认失败，再写生产代码；每项完成后执行定向测试。

---

### Task 1: 登录切换按空闲撤销旧会话

**Files:**
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/auth.rs`
- Test: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/auth.rs`

**Interfaces:**
- Consumes: `AppState::revoke_root_capabilities`, `OperationCoordinator::try_acquire_idle`, `SessionLifecycle::stop`.
- Produces: 一个仅在 idle 时替换内存 token 的登录收尾 helper，供 `auth_login` 调用。

- [ ] **Step 1: 写 RED 测试：busy 时拒绝切换且状态不变。**

在 `auth.rs` test module 中启动本地 session、激活 ROOT capability、持有 coordinator operation，并调用新登录收尾 helper。断言错误等于 `OPERATION_IN_PROGRESS_MESSAGE`，旧 token 仍存在、旧 capability 仍可读取、lifecycle 仍在运行。

- [ ] **Step 2: 运行 RED 测试。**

Run: `cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml -p nwflash-tauri commands::auth::tests::busy_login_replacement_preserves_the_existing_session`

Expected: FAIL，因为 helper 尚未取得 idle lease 并拒绝 busy 状态。

- [ ] **Step 3: 写 RED 测试：空闲切换先撤销再替换 token。**

建立旧 token、运行 lifecycle 和已注册 ROOT capability，调用 helper 写入新 token。断言旧 capability 无效、lifecycle 已停止、token 仅为新值。

- [ ] **Step 4: 实现最小登录收尾。**

将 `auth_login` 的 token 写入替换为 async helper：先 `try_acquire_idle()`，调用 `revoke_root_capabilities(&idle_lease)`，以 `NotStarted` 为可接受结果停止 lifecycle，flush usage，最后持有 token write lock 写入新 token。将 busy 错误映射为 `OPERATION_IN_PROGRESS_MESSAGE`；不要在网络 login 前持有 idle lease。

- [ ] **Step 5: 运行定向测试并提交。**

Run: `cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml -p nwflash-tauri commands::auth::tests`

Commit: `fix(session): revoke capabilities before login replacement`

### Task 2: 撤销固件工件及其私有 staging

**Files:**
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/firmware.rs`
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/lib.rs`
- Test: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/lib.rs`
- Test: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/quick_flash.rs`

**Interfaces:**
- Consumes: `FirmwareArtifactRuntime`, `AppState::revoke_root_capabilities`, `PreparedFirmwareArtifactRuntime`.
- Produces: `FirmwareArtifactRuntime::clear_owned() -> Vec<PathBuf>` that clears the opaque ID and returns only owned staging roots.

- [ ] **Step 1: 写 RED 测试：撤销后旧 artifact ID 不能读取。**

在 `lib.rs` tests 中创建 session、把 owned firmware artifact 置入 runtime、调用 `revoke_root_capabilities`。断言 `get(old_id)` 失败、owned staging 目录消失，且外部输入路径未删除。

- [ ] **Step 2: 写 RED 测试：新会话不能以旧 artifact 建立确认计划。**

在 `quick_flash.rs` tests 中先注册 artifact，撤销/重新激活 scope 后调用 `prepare_firmware_artifact_confirmation`。断言返回“固件提取结果已失效”且 prepared runtime 为空。

- [ ] **Step 3: 运行 RED 测试。**

Run: `cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml -p nwflash-tauri app_state_revocation_discards_firmware_artifact`

Expected: FAIL，因为撤销尚未清除 `firmware_artifacts`。

- [ ] **Step 4: 实现最小 owned 清理。**

为 `FirmwareArtifactRuntime` 增加 `clear_owned`：取走当前 artifact，仅在 `cleanup_staging_root` 为真时返回其 `staging_root`。在 `AppState::revoke_root_capabilities` 的 scope invalidate closure 内调用它并追加到退出后删除列表。不要为工件增加 fingerprint 或 device serial 字段。

- [ ] **Step 5: 运行定向测试并提交。**

Run: `cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml -p nwflash-tauri commands::firmware::tests commands::quick_flash::tests`

Commit: `fix(session): revoke firmware artifacts on session teardown`

### Task 3: Quick Flash 在授权后实时发现唯一目标

**Files:**
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/device.rs`
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/quick_flash.rs`
- Test: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/quick_flash.rs`

**Interfaces:**
- Consumes: `resolve_fastbootd_serial`, `discover_current_device`, `DeviceRuntime`, `PartitionExecutionPlan`.
- Produces: async execution-plan resolver that runs only inside the admitted `run_async` closure.

- [ ] **Step 1: 写 RED 测试：缓存 fastboot snapshot 不足以放行执行。**

为 resolver 提供可注入的实时 discovery seam；用 snapshot `FAST-A` 和实时结果 `MultipleDevices` 调用它。断言返回 `DeviceUnavailable`，且没有构造 flash command。

- [ ] **Step 2: 写 RED 测试：实时单设备覆盖预览 serial。**

用计划 preview serial `FAST-A` 和实时 fastbootd `FAST-B` 调用 resolver。断言返回计划的 serial 为 `FAST-B`，其后 command 仅含 `-s FAST-B`。

- [ ] **Step 3: 运行 RED 测试。**

Run: `cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml -p nwflash-tauri commands::quick_flash::tests::execution_`

Expected: FAIL，因为现有同步 resolver 只调用 `DeviceRuntime::active_*_serial()`。

- [ ] **Step 4: 实现最小实时 resolver。**

将 `resolve_execution_plan` 改为 async，并接收 `OperationContext` 与 cancellation。Fastboot 分支调用 `resolve_fastbootd_serial(..., false, ...)`，该路径运行 `fastboot devices`、拒绝多个设备并检查 `is-userspace`；ADB Root 分支调用实时 ADB discovery，要求 `AdbConnected` 后更新 runtime。仅在 resolver 成功后调用 `QuickFlashService::retarget_execution_plan`，不比较旧/新 serial。

- [ ] **Step 5: 对 post-action 使用相同实时 fastboot resolver。**

在 slot switch 和 reboot 前调用同一实时 fastbootd resolver，而不是从缓存 snapshot 读取 serial；保留每项操作的 cancellation、终端状态和错误投影。

- [ ] **Step 6: 运行定向测试并提交。**

Run: `cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml -p nwflash-tauri commands::quick_flash::tests`

Commit: `fix(quick-flash): resolve unique device at execution time`

### Task 4: 敏感修复收尾验证

**Files:**
- Modify: `.gitignore`
- Test: Rust workspace、React unit tests、.NET solution tests。

- [ ] **Step 1: 忽略可再生 Node/Tauri 输出。**

在 `.gitignore` 添加 `**/node_modules/`、`src/Nwflash.Desktop/dist/` 与 `src/Nwflash.Desktop/src-tauri/gen/`。不要删除它们；保留 release、resources、lockfiles、`.superpowers` 与 artifacts。

- [ ] **Step 2: 运行最终验证。**

Run: `cargo fmt --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --check`

Run: `cargo clippy --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --workspace --all-targets -- -D warnings`

Run: `cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --workspace --no-fail-fast`

Run: `npm --prefix src/Nwflash.Desktop test`

Run: `dotnet test VivoKsu.slnx --no-restore`

- [ ] **Step 3: 检查提交边界。**

Run: `git diff --check`

Run: `git diff --cached --name-only`

Stage only project source, tests, configs, lockfiles, resources, scripts, packaging and current architecture docs; never stage ignored build output, logs or `.superpowers`.

- [ ] **Step 4: 分组提交。**

Create focused commits for Tauri migration foundation, sensitive session/target remediation, packaging/release inputs, and architecture documentation. Verify each staged diff before committing.
