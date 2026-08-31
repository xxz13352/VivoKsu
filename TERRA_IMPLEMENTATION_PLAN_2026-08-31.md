# VivoKsu / NWFlash — GPT-5.6 Terra 执行计划

日期：2026-08-31（Asia/Shanghai）

## 1. 目标与完成定义

本计划供 GPT-5.6 Terra 直接执行。目标是从当前接力状态继续完成：

1. 修复 `sealed-spool-facade` 当前未提交、不可编译的 protection WIP。
2. 完成 sentinel-attested metadata facade 的安全边界与测试。
3. 为重启后丢失 live capability 建立明确、持久化、可审计的 loss 闭环。
4. 依次集成 producer 静态类型、process trace adapter、producer → spool → HTTP、最终 spawn authority。
5. 在最新 integration tip 上跑完整非部署门禁。
6. 仅在用户再次明确授权后，执行 VMProtect GUI、签名、安装器和真机流程。

完成态不等于“真实发布完成”。在 VMProtect GUI、protected runtime/CRC、Authenticode、NSIS、安装/卸载、登录/heartbeat、真机 smoke 完成前，禁止使用“已保护”“可正式发布”等表述。

## 2. 当前可信状态

### 主线

- 集成工作树：`.worktrees/integration-staging`
- 分支：`codex/integration-staging`
- HEAD：`4f062f5 fix(application): bind dispatch authority to cancellation`
- 前一提交：`33dc9aa fix(application): seal operation dispatch authority`
- 当前 integration 工作树应保持 clean。

已验证的 operation dispatch guard：

- focused：31/31
- `nwflash-application` lib：42/42
- doctest：2/2
- rustfmt：通过
- Clippy `-D warnings`：通过

### 当前执行分支

- 工作树：`.worktrees/sealed-spool-facade`
- 分支：`codex/sealed-spool-facade`
- checkpoint HEAD：`51072da feat(trace): add attested metadata spool facade`
- 该 commit 已 rebase 到 `4f062f5`。
- 当前存在未提交 WIP：
  - `M src/Nwflash.Desktop/src-tauri/crates/nwflash-protection/src/trace_redaction.rs`
  - 当前 diff 约 `+65/-5`。

不要 reset、clean、checkout 或丢弃这份 WIP。应基于它向前修复。

### 当前 WIP 的确定性编译错误

命令：

```powershell
cargo check --locked --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml -p nwflash-protection
```

当前共 6 个错误：

1. 缺少 `TraceCredentialSentinelContext::from_event`。
2. 缺少 `TraceCredentialSentinelContext::from_run`。
3. 缺少 `sealed_metadata_entity_tag`。
4. 缺少 `bind_sentinel_metadata_identity`。
5. `TraceCredentialSentinelContext::for_upload` 仍接收 `TraceId`，调用方已改为传 `&SealedTraceUpload`。
6. 上述缺失函数导致 `SentinelAttestedTraceUpload::from_run` 无法编译。

### 必须保留的其他脏工作树

- `.worktrees/producer-sentinel-static`
  - `M nwflash-application/src/trace_producer.rs`
  - 当前仅 compile-fail RED。
- `.worktrees/process-trace-adapter`
  - `M nwflash-application/src/lib.rs`
  - `?? nwflash-application/src/process_trace.rs`
- `.worktrees/planc-producer-spool-adapter`
  - `?? nwflash-infrastructure/tests/trace_metadata_adapter.rs`
- `.worktrees/refresh-spawn-race-fix`
  - 修改 `device.rs`、`device_identity.rs`、`root_ota.rs`

禁止对这些工作树执行 `git reset --hard`、`git clean`、批量 checkout 或 prune。

### 本机工具链

- Rust/Cargo：`C:\Users\17254\AppData\Local\CodexRust\cargo\bin`
- Rust：1.98.0 stable，MSVC x64
- Visual Studio Build Tools：
  - `C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools`
- VMP：
  - `C:\Users\17254\Downloads\VMProtect Lite v3.10.4 Build 2668 (1)`
  - 已确认包含 `VMProtect.exe`、头文件、Windows x64 DLL/LIB。

建议每个 Rust 命令先加载：

```powershell
$TaskVsShell = 'C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\Common7\Tools\Launch-VsDevShell.ps1'
& $TaskVsShell -Arch amd64 -HostArch amd64 -SkipAutomaticLocation
$TaskRustRoot = Join-Path $env:LOCALAPPDATA 'CodexRust'
$env:RUSTUP_HOME = Join-Path $TaskRustRoot 'rustup'
$env:CARGO_HOME = Join-Path $TaskRustRoot 'cargo'
$TaskCargo = Join-Path $env:CARGO_HOME 'bin\cargo.exe'
```

## 3. 不可变安全约束

1. `SentinelAttestedTraceUpload` 必须是 facade/producer sink 接受的静态类型；不要添加 raw `SealedTraceUpload` shim。
2. metadata view 必须由 protection 内部从具体 attested upload 派生，外部不能构造或覆盖 entity、parent、run ID、timestamp、body hash/length。
3. receipt 必须同时绑定：
   - session/logical record identity；
   - concrete upload ID；
   - canonical wire body hash和长度；
   - 可重算的 metadata identity。
4. receipt 必须显式拒绝 `high_risk == true`。
5. 不要给 `TraceCredentialSentinelInput` / receipt 再增加 32 字节字段；现有 fixed-size 探针要求保持紧凑。metadata digest 应折叠进 session identity。
6. later-chunk 的 `run_id`、parent event、`created_at_ms` 必须被 receipt 认证，不能只依赖不可变内存字段。
7. ACK 必须在一个锁、一次原子 persist 中完成 accepted/rejected/unacknowledged/remediation CAS。
8. stale revision 不得删除、覆盖或错误报告新 revision 的 remediation。
9. metadata-only spool 不能被描述为可恢复 HTTP body；必须实现明确的 restart loss 语义或真正的 payload vault。
10. 不要通过删除 retired attempts 简单规避 256 上限；这会丢失 sealed ID 去重与 loss attempt count。

## 4. 阶段 A — 完成当前 protection WIP

工作树：`.worktrees/sealed-spool-facade`

### A1. 实现缺失 helper

在 `nwflash-protection/src/trace_redaction.rs` 完成：

- `TraceCredentialSentinelContext::from_event(&RedactedTraceEvent)`
  - 使用完整 event `redaction_summary`，包括 event metadata 与输出流。
  - identity 至少绑定 event ID、run ID、sequence、started time、chunk identities/hash 与每类 redaction count。
- `TraceCredentialSentinelContext::from_run(&RedactedTraceRun)`
  - 使用 run 的完整 redaction summary。
  - identity 至少绑定 run ID、started time 与每类 redaction count。
- `TraceCredentialSentinelContext::for_upload(&SealedTraceUpload, body)`
  - 从当前 upload 重算 metadata binding。
  - 使用 `bind_sentinel_metadata_identity(base_identity, metadata_identity)` 折叠后再进入 sentinel leaf。
- `sealed_metadata_entity_tag`。
- `bind_sentinel_metadata_identity`。
- `trace_event_identity` 与 `trace_run_identity`。

### A2. 完成 metadata identity

保持当前已开始的拆分：

- `metadata_items()`：只创建 canonical metadata，不递归调用 body verification。
- `metadata_binding_identity()`：
  - scoped run/event/chunk：hash entity、item ID、trace/run ID、parent entity/ID、created time 和 counts；
  - unscoped output-only：允许 HTTP body 继续 fail-safe 使用，但 `metadata_view()` 必须返回 `MissingTraceContext`；
  - mixed/缺失 context：fail closed。
- `verify_credential_sentinel_receipt()`：
  - 从当前 metadata sidecar 重算 binding；
  - 与 receipt 中的 bound session identity 常量时间比较；
  - 保留 body hash/length/upload ID/high-risk 校验。

### A3. 完成 run-only attested 构造

保留并完成：

```rust
SentinelAttestedTraceUpload::from_run(
    input: TraceRunText<'_>,
    secrets: &ExactSecretSet,
)
```

要求：

- 内部执行 `RedactedTraceRun::try_new`；
- 内部生成 fresh UUIDv7 upload ID；
- 不向调用方暴露 receiptless 中间态；
- 通过 sentinel leaf 后再返回 capability；
- high-risk run 必须失败。

### A4. 必补测试

至少新增：

1. `attested_run_upload_exposes_exact_run_metadata`
2. `run_constructor_redacts_secrets_and_rejects_high_risk`
3. `event_receipt_counts_event_metadata_redactions`
4. `later_chunk_sidecar_tampering_invalidates_receipt`
   - 分别覆盖 run ID 或 created time 被修改。
5. `unscoped_output_attempt_still_cannot_claim_metadata`
6. 现有 `high_risk_receipt_cannot_become_an_attested_upload` 保持通过。

### A5. 阶段验证

```powershell
& $TaskCargo fmt --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --all -- --check
& $TaskCargo test --locked --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml -p nwflash-protection --lib trace_redaction::tests
& $TaskCargo test --locked --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml -p nwflash-protection --doc
& $TaskCargo clippy --locked --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml -p nwflash-protection --all-targets -- -D warnings
git diff --check
```

通过后提交独立 commit，不要 amend `51072da`。

建议提交标题：

```text
fix(trace): bind attested uploads to canonical metadata
```

## 5. 阶段 B — 完成 facade/store 低层契约

### B1. 保留已经完成的修复

`51072da` 已包含并验证：

- exact attempt claim；
- owner/client-version/body/item snapshot 绑定；
- stale credential rejection 不再报告伪 remediation；
- mixed ACK 单锁单 persist；
- high-risk receipt 显式拒绝；
- metadata-only spool 不落 sealed body。

### B2. 新增缺失的低层测试

为 `TraceSpoolStore` 增加直接测试：

1. `peek_due_attempts` 不执行 expiry/recovery/任何 persist。
2. `apply_validated_ack_cas` 对以下输入零变更并失败：
   - duplicate key；
   - accepted/rejected overlap；
   - missing dispatched member；
   - unknown member；
   - credential rejection 指向非 chunk。
3. persist 故障时，mixed ACK 保留旧 manifest，不能部分 accepted。

### B3. 解决 completed attempt 容量

现状：retired manifests 永不压缩，owner generation 成功约 256 次后会永久注册失败。

禁止只删除 Retired manifest。推荐实现：

- 新增有界 completed-attempt ledger/tombstone；
- 至少保留：attempt ID、sealed upload ID、相关 trace IDs、完成时间、每 trace attempt 计数；
- duplicate 检查必须同时查 active attempts 与 completed ledger；
- loss diagnostic 必须计入 compacted history；
- ledger retention 与 7 日 trace retention 一致；
- manifest schema 变化必须版本化，并提供旧版本迁移或 fail-closed 明确策略；
- 原子 replace 失败必须保留旧文件。

必补压力测试：

- 同一 owner generation 连续完成 300+ 次，不出现永久 `Storage`；
- compact 后旧 sealed body 仍不能重新注册；
- loss `attempt_count` 不因 compact 低报；
- reopen 后语义一致。

建议独立提交：

```text
fix(trace): bound completed attempt history
```

## 6. 阶段 C — 建立 restart loss 闭环

推荐先实现“明确 durable loss”，不要伪装成 body recovery。

### C1. 范围

- 每个注册 attempt 持久化 process/build epoch 或 process nonce hash。
- facade reopen 时：
  - 同一 process epoch 的 live capability 仍可继续；
  - 上一 process epoch 的 pending/inflight metadata 视为 orphan；
  - 写 durable loss tombstone，reason 固定为 `live-seal-lost-on-restart`；
  - tombstone 原子成功后，才能删除/退休 orphan metadata；
  - loss 写入失败则 fail closed，不得静默删除。
- loss 只记录 text-free identity/count/hash，不落原始或 redacted body。

### C2. 崩溃矩阵

必须测试：

1. body/capability 生成后、metadata register 前崩溃。
2. metadata persist 后、capability 返回前崩溃。
3. claim 前崩溃。
4. Inflight/HTTP 前崩溃。
5. HTTP 已发、ACK persist 前崩溃。
6. loss tombstone persist 失败。
7. loss 成功但 manifest replace 失败。

每一种都必须得出唯一结果：可继续、durable loss、或 fail-closed；禁止“可能重复又可能丢失”的未定义状态。

建议提交：

```text
fix(trace): account for orphaned live seals after restart
```

## 7. 阶段 D — 集成 producer 与 process adapter

严格按以下顺序：

### D1. producer sentinel-static

工作树：`.worktrees/producer-sentinel-static`

- rebase 到最新 `codex/integration-staging`；
- `TraceMetadataSink::append_upload` 与 `TraceRunHandle::append_upload` 只接收 `SentinelAttestedTraceUpload`；
- 禁止 raw shim；
- 保留 sequence reservation、identity lease、batch atomic 语义；
- 使用 compile-fail doctest，不引入 trybuild，不修改 Cargo.lock。

### D2. process trace adapter

工作树：`.worktrees/process-trace-adapter`

- rebase 到已集成 producer-static 的主线；
- 消除现有 E0053/E0308；
- 保持 binary stdout 排除、完整流扫描、termination/cap/high-risk 失败零上传；
- pending retry 必须使用相同 upload IDs。

### D3. producer → spool adapter

工作树：`.worktrees/planc-producer-spool-adapter`

- 接入 facade，而不是 raw `TraceSpoolStore::begin_dispatch`；
- owner/login generation/build/token scope 必须从一次验证后的 session snapshot 原子获取；
- 401/403 只暂停 exact owner generation；
- 426 是全局 client-version gate；
- shutdown 使用有界 deadline；
- 已过期 live capability 只能走 durable loss，不得重建假 body。

每个子阶段都先 focused，再三 crate 闭包，然后独立 commit；不要跨工作树同时编辑相同文件。

## 8. 阶段 E — 最终 spawn authority

工作树：`.worktrees/refresh-spawn-race-fix`

- rebase 到含 `33dc9aa + 4f062f5` 的最新 integration；
- 在真实 device/root OTA 同步 OS spawn 点使用 `with_running_dispatch`；
- operation context 自取消后不得 dispatch；
- stale/drop context 不得 dispatch；
- 不要把检查放在 spawn 之前的异步阶段；最后一步必须在同一 admission guard 内。

## 9. 阶段 F — 集成与完整非部署门禁

每个 feature commit 先进入 `codex/integration-staging`，最终 tip 上执行：

### Rust

```powershell
& $TaskCargo test --locked --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --workspace --no-fail-fast
& $TaskCargo clippy --locked --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --workspace --all-targets -- -D warnings
& $TaskCargo fmt --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --all -- --check
git diff --check
```

### 桌面前端

```powershell
npm ci --prefix src/Nwflash.Desktop
npm run test --prefix src/Nwflash.Desktop
npm run build --prefix src/Nwflash.Desktop
```

### Cloudflare

按 `cloudflare/README.md` 对 shared/admin/user/website 执行：

- npm ci
- Node/Vitest
- Workerd
- typecheck
- Wrangler dry-run
- 浏览器测试（若依赖已恢复）

禁止 deploy。

### VMP 自动门禁

VMP SDK 根目录：

```text
C:\Users\17254\Downloads\VMProtect Lite v3.10.4 Build 2668 (1)
```

只执行非 GUI、非签名、非安装的验证脚本。使用 PowerShell 7.4+，按 `scripts/vmp/README.md` 与 `docs/release/tauri-vmp-signing-runbook.md` 的当前实现为准。

## 10. 阶段 G — 需要用户再次授权的操作

以下操作不要自行执行：

- 启动 VMProtect GUI 并保护生产二进制；
- Authenticode 签名；
- 生成/安装 NSIS；
- 卸载测试；
- 生产 Cloudflare deploy；
- 登录真实账号/heartbeat；
- 连接或操作真机；
- 创建公开发布物。

在进入本阶段前，向用户汇报：最终 commit、完整 gate 结果、已知残余风险、预计磁盘占用和具体将执行的外部副作用。

## 11. Terra 执行纪律

1. 从 `.worktrees/sealed-spool-facade` 当前 WIP 开始，先修复编译，不要重做或丢弃。
2. 任何失败先记录精确命令、exit code、首个根因；不要用宽泛 clean 解决。
3. 不运行 `git reset --hard`、`git clean`、批量 worktree prune。
4. 不删除其他工作树的 target、未提交文件或 `.wrangler`。
5. 只 stage 本阶段明确文件。
6. 每个 commit 前必须有 fresh 测试证据。
7. rebase 冲突以最新 integration 为底，保留 `run_count/event_ids/event_bindings`、trace HTTP transport 和 operation guard。
8. 不要把 source-ready/gate-ready 表述成 VMProtect-protected 或 release-ready。
9. 每完成一个阶段，更新本文件的执行记录或另写 handoff，包含 commit SHA、测试计数、未解决 blocker。

## 12. 第一条建议执行命令

```powershell
Set-Location 'C:\Users\17254\Desktop\存档\TOOL\VivoKsu 工具\.worktrees\sealed-spool-facade'
git status --short --branch
git diff -- src/Nwflash.Desktop/src-tauri/crates/nwflash-protection/src/trace_redaction.rs
```

先修复阶段 A 的 6 个编译错误，完成 focused tests 后再进入任何其他工作树。

## 13. 本次执行记录（2026-08-31）

已完成并集成到 `codex/integration-staging`：

- `33dc9aa` / `4f062f5`：operation dispatch authority；取消令牌绑定修复已包含 self-cancel 回归。
- `6864812` / `5a67b8d`：attested metadata spool facade、Run-only attested upload、canonical metadata binding、stale remediation/high-risk 修复。
- `0c2716b`：mirror 启动失败统一为脱敏外部工具错误，并清理两处既有 Clippy dead-code 门禁。
- `62b9112`：capability boundary 测试设置固定合法 build ID，区分 probe unavailable(43) 与 missing build identity(46)。

已验证：

- Rust workspace `cargo test --workspace --no-fail-fast`：通过；所有 workspace 测试通过。
- Rust workspace Clippy `--all-targets -- -D warnings`：通过。
- Rustfmt 与 `git diff --check`：通过。
- 桌面前端：UI 180/180、capability 5/5、生产构建通过。
- Cloudflare API：Node 77/77、Workerd 179/179、typecheck/dry-run 通过。
- 管理后台：unit 135/135、Workerd 51/51、Chromium 31/31 通过。
- 用户门户：UI 34/34、Workerd 27/27、typecheck/dry-run 通过。
- 官网 Wrangler dry-run 通过。
- VMP SDK verifier、六叶 link/layout contract、protected profile、Tauri release fixture、PowerShell runtime/protected behavior contract 通过。
- VMP GUI、签名、部署、安装、登录和真机均未执行。

仍未完成：

- durable sealed body/restart recovery 或明确 orphan → durable loss 闭环；
- completed-attempt 历史容量与去重账本；
- producer sentinel-static、process trace adapter、producer → spool → HTTP、最终 Tauri spawn 接线；
- 手工 VMProtect Lite 保护、compiler log/marker review、protected runtime/CRC、Authenticode、NSIS、安装卸载和真机 smoke。
