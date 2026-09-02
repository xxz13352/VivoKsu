# NWFlash 当前任务接力

更新时间：2026-09-02（第二轮接力完成）
分支：`codex/vmp-release-completion`

## 当前目标

完善工具各功能的客户端日志和服务器回传，特别是固件提取的分区级进度；保证上传失败后可由本地 durable spool 稍后重试，并让管理端长期保存和查看这些日志。

## 用户已确认的策略

- 固件提取采用“阶段级 + 分区级”日志。
- 客户端上传全部操作日志和详情。
- 校验哈希不上传。
- 不上传密码、Token、Cookie、私钥、签名密钥或签名 URL。
- 设备序列号和本地路径按用户确认明文上传。
- 上传失败后稍后重试。
- 服务器长期保留 V1 使用日志。
- UI 需要使用 Fates/Taste 风格统一。
- 不增加功能性注释。

## 本轮完成的工作

### Rust 工具链（环境已解决）

本机存在可用的 Rust 工具链，位于 `C:\Users\17254\AppData\Local\CodexRust\`。使用前需设置：

```bash
export RUSTUP_HOME="C:/Users/17254/AppData/Local/CodexRust/rustup"
export CARGO_HOME="C:/Users/17254/AppData/Local/CodexRust/cargo"
export PATH="/c/Users/17254/AppData/Local/CodexRust/cargo/bin:$PATH"
```

版本：cargo/rustc 1.98.0 stable（msvc）。

### 编译修复（`cargo check --workspace` 现已通过）

- `firmware.rs`：修复 `firmware_extract_vivo_local` 中 `progress` 被 move 进闭包后再次使用的 E0382 所有权错误（引入 `progress_for_extraction` 克隆）。
- `lib.rs`：`normalize_operation_log_message` 的 `level` 参数改为 `_level`（删除服务器探测过滤后不再使用）。
- `cargo fmt` 已应用全部工作区格式修正。

### 测试修复（`cargo test --workspace` 全绿）

- `lib.rs` 测试 `operation_log_omits_routine_server_probe_messages` 改为 `operation_log_keeps_routine_server_probe_messages`，断言与新行为一致（服务器探测消息保留、OTA 字样仍归一化为“固件”）。

### 固件提取进度（前一轮遗留，本轮验证）

`FirmwareProgressDto` 含分区级字段（`currentPartitionIndex/totalPartitions/completedPartitions/successfulPartitions/failedPartitions/skippedPartitions`），覆盖本地 VIVO、远程 ZIP、远程 payload、本地 payload。已知限制：失败/跳过分类回调尚未接入，通常为 0；实际错误进入统一操作失败日志。

### 客户端日志显示（前一轮遗留，本轮验证）

服务器探测/请求/OTA 解析日志不再隐藏；仍过滤空消息和“准备 VIVO 线刷”标题。桌面 22 个测试文件 / 182 个测试通过（含 2 个本轮新增的固件提取进度测试，断言 `X/N` 分区序号与成功/失败/跳过统计，见 `FirmwareExtractPage.test.tsx`）。

### 使用日志详情（前一轮遗留，本轮验证）

`UsageLogDetail`/`UsageLogEntry.details` 已随 durable spool 上传（500 条上限、同级同消息去重）。

### Cloudflare V1 使用日志（前一轮遗留，本轮补测试）

`usage_logs.details_json` 列已加入 schema 与 API/管理端查询。本轮新增回归测试 `cloudflare/test/usage-log-details.workerd.test.ts`（5 个用例）：

1. 详情持久化 + 管理端查询返回 `details_json`。
2. 旧客户端缺失/非法详情按 `[]` 兼容（对象、非对象成员、null）。
3. 500 条截断 + 单条消息 16,384 字节上限。
4. 非法 timestamp/空白 level/空消息的归一化（NaN→0、空 level→Info、空消息丢弃、非字符串消息 String 化）。
5. event_key 幂等（重试不重复插入、首次详情保留）。

Cloudflare 全部测试通过：workerd 套件 7 文件 184 测试（含新 5 个）；标准套件 3 文件 77 测试；admin workerd 51 测试。`npm run typecheck` 通过。

### 未协调路径盘点（本轮完成审计）+ 三处新协调

审计结论（全部 commands 模块）：

- 已协调：firmware 全部提取、quick_flash execute 系列、safe_flash、root_preflight/install/patch/automatic、root_ota_extract_images、mirror_start、files_* 全部、resource_install、driver_reinstall、device_reboot 系列、auth 登录收尾、partitions_refresh/execute。
- 纯状态读取（无需协调）：`resource_inventory`、`software_status`、`mirror_status`、`session_state`、`version_check`、`operation_logs_snapshot/clear`、`online_sessions`、`partitions_cached_snapshot`、`partitions_prepare_*`（纯计划构建）、`quick_flash_prepare_*`（纯计划/确认）。
- 本轮新增协调（原先未协调、用户可感知耗时）：
  1. `firmware_inspect_local` 非 payload 分支（ZIP/目录/gzip-tar 固件检查）→ `inspect_local_or_payload` 尾部改为 `run_async("检查本地固件")`，阶段日志含“本地固件检查完成：发现 N 个分区”。
  2. `firmware_inspect_line_flash_package` → 新增 `State` 参数并 `run_async("检查线刷固件包")`。无前端调用者（命令已注册但当前 UI 未用），Tauri 自动注入 State，无需前端改动。
  3. `root_select_image` → 提取 `inspect_root_image_through_coordinator` 辅助函数并 `run_async("检查 ROOT 镜像")`。前端仅传 `{ kind }`，无需改动。

- 新增 Rust 测试证明以上路径产生 usage log entry + details：
  - `commands::firmware::tests::local_firmware_inspection_records_usage_details_for_non_payload_sources`
  - `commands::root::tests::root_image_inspection_records_usage_details_through_the_coordinator`

### 仍不协调（有意保留，低风险）

- `device_refresh`：走 `AdmissionCheckedExecutor`（admission 感知）+ `try_acquire_idle`，但只为守门不做 run_async 操作记录；失败才写本地操作日志。属高频状态刷新，若纳入 usage log 会产生大量噪音。保持现状。
- `quick_flash_inspect_image`：同步读取镜像头，快，纯预检。保持现状。

## 已验证

Rust（全部通过）：

- `cargo fmt --check` ✅
- `cargo check --workspace` ✅
- `cargo test --workspace` ✅（全部 crate 绿色，含新增测试）

桌面端：

- `npm run build` ✅
- `npm run test:ui` ✅（22 文件 / 182 测试，含 2 个新增进度测试）

Cloudflare：

- `npm run typecheck` ✅
- 标准 vitest（3 文件 77 测试）✅
- workerd 全套（7 文件 184 测试，含 5 个新增详情回归）✅
- admin workerd（51 测试）✅

## 下一步

1. 生产 D1 部署前必须先执行（对应迁移文件 `cloudflare/web/migrate-usage-log-details.sql`）：

   ```sql
   ALTER TABLE usage_logs ADD COLUMN details_json TEXT NOT NULL DEFAULT '[]';
   ```

   然后 `npm run deploy`（API Worker + Web Worker），验证 `/api/usage-logs` 管理端查询返回 `details_json`。

2. 如果要上传外部进程 stdout/stderr，先实现安全的结构化命令观测适配器；不要直接把任意原始 stdout 全量上传。必须继续过滤校验哈希、凭据和签名 URL。

3. 固件提取的失败/跳过分类回调目前缺失（`failed_partitions/skipped_partitions` 恒为 0）。如需真实统计，需扩展 `FirmwareExtractService` 的提取回调协议增加 terminal 分类事件。

4. Release EXE 构建验证（`cargo build --release` 或 Tauri bundle）尚未在本轮执行——本轮只跑了 dev profile 的 check/test。

## 当前工作区注意事项

不要执行以下操作：

- `git reset`
- `git clean`
- 删除 `.workbuddy`
- 删除 Release EXE
- 回滚用户已有的无关修改

当前已有用户/前序工作修改，尤其包括：

- `.gitignore`
- `src/Nwflash.Desktop/src/styles/app.css`
- `src/Nwflash.Desktop/src/styles/unified.css`
- `.workbuddy/`

这些改动应保留并与后续修改一起审查。

## 相关文件

- Rust 工作区：`src/Nwflash.Desktop/src-tauri/Cargo.toml`
- 操作协调器：`src/Nwflash.Desktop/src-tauri/crates/nwflash-application/src/operation_coordinator.rs`
- 固件命令：`src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/firmware.rs`
- 固件页面：`src/Nwflash.Desktop/src/pages/FirmwareExtractPage.tsx`
- V1 API：`cloudflare/src/index.ts`
- 管理端 API：`cloudflare/web/src/index.ts`
- V1 数据库结构：`cloudflare/web/schema.sql`
- D1 迁移：`cloudflare/web/migrate-usage-log-details.sql`
- 新增 workerd 回归：`cloudflare/test/usage-log-details.workerd.test.ts`
