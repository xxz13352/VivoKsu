# VMP、本地完整性与网络验证加固 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:test-driven-development for each behavior change, superpowers:executing-plans to execute this plan, and superpowers:verification-before-completion before claiming completion.

**Goal:** 在设备功能保持纯本地的前提下，为 Rust/Tauri 客户端加入服务器签名租约、API 证书链与 SPKI 双重校验、VMP CRC/叶函数保护、篡改立即上报退出，以及心跳失败等待当前任务完成后退出。

**Architecture:** 新增纯同步 `nwflash-protection` crate，负责协议验签、租约约束、操作准入、心跳分类、完整性决策和 VMP SDK 适配；Cloudflare Worker 只签发短期租约/签名 pin 清单并接收最小遥测；`nwflash-infrastructure` 提供专用 pinned API client；Tauri 层持有 session capability 和 Rust 退出监督器。React 不接触 token、签名密钥或授权决策。

**Tech Stack:** Rust 2021、Tauri 2、Tokio、rustls 0.23、reqwest、Ed25519、Cloudflare Workers/TypeScript、Vitest、VMProtect 3.10.4 x64 SDK。

## Global constraints

- VMP SDK、许可证、代码签名证书和 Ed25519 私钥不得提交仓库。
- 不将 API 地址混淆、前端状态、React event 或 VMP 单一检测当作授权边界。
- 网络/API 保护不得改变固件、ROM 和其他第三方下载 client。
- 正在执行设备写入时，心跳失败不得取消任务或杀死子进程；只拒绝新任务并等待 coordinator idle。
- 本地明确篡改在安全点最多尝试 750 ms 上报，然后无条件由 Rust 退出。
- 每个行为先写失败测试，确认 RED 后实现；每项完成后运行定向测试。

---

### Task 1: 新建纯 Rust protection crate 与签名租约协议

**Files:**
- Modify: `src/Nwflash.Desktop/src-tauri/Cargo.toml`
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-protection/Cargo.toml`
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-protection/src/lib.rs`
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-protection/src/lease.rs`
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-protection/src/decision.rs`
- Test: `src/Nwflash.Desktop/src-tauri/crates/nwflash-protection/tests/signed_lease.rs`
- Test: `src/Nwflash.Desktop/src-tauri/crates/nwflash-protection/tests/decision_matrix.rs`

**Interfaces:**
- `SignedEnvelope { lease_payload, lease_signature }`
- `LeaseClaims { version, kind, username, token_sha256, client_version, build_id, process_nonce, session_id, sequence, issued_at, expires_at }`
- `verify_signed_lease`, `accept_login_lease`, `classify_heartbeat_lease`, `admit_local_operation`, `dispatch_protection_decision`.

- [ ] Write RED tests for valid signature, wrong signature, modified payload, malformed base64/JSON, expiry, future issue time, wrong token digest/client/build/nonce/session/kind, and sequence rollback.
- [ ] Run `cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml -p nwflash-protection` and confirm compilation/test failure before implementation.
- [ ] Implement base64url envelope verification over the original payload ASCII bytes with `ed25519-dalek`; parse only after signature success; compare token by SHA-256 digest and enforce a bounded clock skew.
- [ ] Implement closed decision enums and an encoded integer selector with illegal-selector fail-closed behavior. Apply `#[inline(never)]` and stable `#[export_name]` wrappers only to the pure leaf boundaries.
- [ ] Add `zeroize` wrappers for token digest/intermediate buffers and tests that replacement/drop paths execute without exposing secrets through `Debug`.
- [ ] Run crate tests and commit `feat(protection): verify signed session leases`.

### Task 2: Cloudflare 签发租约、pin 清单与接收完整性遥测

**Files:**
- Modify: `cloudflare/package.json`
- Modify: `cloudflare/package-lock.json`
- Modify: `cloudflare/src/index.ts`
- Modify: `cloudflare/wrangler.toml`
- Modify: `cloudflare/web/schema.sql`
- Create: `cloudflare/src/security.ts`
- Create: `cloudflare/test/security.test.ts`
- Modify: `cloudflare/API.md`
- Modify: `cloudflare/README.md`

**Interfaces:**
- Secret: `SESSION_SIGNING_PRIVATE_KEY_PKCS8` containing base64url PKCS#8 Ed25519 private-key DER.
- `/api/login` returns `lease_payload` and `lease_signature`; request includes `process_nonce`, `build_id`, and `client_version`.
- `/api/heartbeat` accepts `sequence` and returns the next signed heartbeat lease.
- `GET /api/security/pins` returns a signed pinset envelope.
- `POST /api/integrity/report` accepts only allowlisted, size-bounded telemetry.

- [ ] Add Vitest and RED unit tests using a deterministic test key: envelope verification, field tampering, missing signing secret fail-closed, monotonic heartbeat sequence, signed pinset, oversized/unknown telemetry rejection, IP rate limiting, and event-id idempotency.
- [ ] Run `npm --prefix cloudflare test` and confirm RED.
- [ ] Implement WebCrypto Ed25519 PKCS#8 import/signing over base64url payload text. Never log or return the private key.
- [ ] Extend login/heartbeat response contracts and bind every claim to user, token digest, build, client version, process nonce, session and sequence.
- [ ] Implement signed two-pin rotation payload with strict host/version/time fields.
- [ ] Add D1 `integrity_events` and `integrity_rate_limits` tables; implement anonymous/authenticated report validation, strict body limit, enum allowlist, event idempotency and IP window limit.
- [ ] Update Worker environment typing/docs and add a deployment preflight that fails if the signing secret is absent.
- [ ] Run Worker tests/typecheck and commit `feat(api): sign leases and accept integrity telemetry`.

### Task 3: NWflash API 专用 WebPKI + SPKI pinned TLS client

**Files:**
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/Cargo.toml`
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/src/pinned_tls.rs`
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/src/lib.rs`
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/src/api_client.rs`
- Test: `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/tests/pinned_tls.rs`
- Test: `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/tests/api_contract.rs`

**Interfaces:**
- `PinnedApiClient::new(ApiTlsPolicy)` with exact host `api.nwflash.cc.cd`.
- Built-in SHA-256 SPKI pins for current leaf and WE1 intermediate.
- Signed cached pinset accepted only after public-key verification, signed time checks, the release embedded version floor, and current-process high-water checks. No tamper-proof cross-start monotonic storage is claimed for an attacker-controlled host; Task 8 raises the floor/key for releases.
- `CloudflareError::Integrity(IntegrityFailure)` distinguishes pin/lease failures from transport failures.
- Custom root/pin/resolver/key and unpinned HTTP adapters are debug/test-only; release exports only the exact production pinned path.

- [ ] Write RED tests for valid chain+pin, valid chain+wrong pin, private proxy root, wrong DNS, expired certificate, proxy environment ignored, signed pin rotation, tampered cache and version rollback.
- [ ] Run the targeted infrastructure tests and confirm RED.
- [ ] Implement a rustls `ServerCertVerifier` that delegates WebPKI hostname/time/chain and TLS signature checks first, then parses leaf/intermediates and matches SHA-256 of SubjectPublicKeyInfo.
- [ ] Build reqwest with preconfigured rustls, `no_proxy()`, no redirects, no invalid-certificate bypass and no TLS key logging; enforce HTTPS and exact host for API endpoints and classify every 3xx as endpoint integrity failure.
- [ ] Persist only the signed public envelope using same-directory atomic replacement, verify signature/host/time/release-floor/current-process-high-water on load/use, and preserve independent clients for firmware/ROM downloads.
- [ ] Update login/heartbeat DTOs for envelopes and classify pin failures as integrity events.
- [ ] Run targeted tests and commit `feat(network): pin nwflash api certificates`.

### Task 4: VMP SDK 适配、CRC 与黑盒叶函数边界

**Files:**
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-protection/Cargo.toml`
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-protection/build.rs`
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-protection/src/vmp.rs`
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-protection/tests/vmp_probe.rs`
- Create: `scripts/vmp/README.md`
- Create: `scripts/vmp/verify-sdk.ps1`

**Interfaces:**
- Feature `vmp-sdk`; environment `NWFLASH_VMP_SDK_ROOT` points to external x64 Include/Lib root.
- `IntegrityProbe` trait and `VmpIntegrityProbe` production implementation.
- `verify_image_integrity` consumes probe output; default/test build uses an injected deterministic probe, not a security-success hardcode in protected release.

- [ ] Write RED tests for valid/invalid CRC, debugger telemetry classification, injected probe, no-feature behavior and protected-feature missing-SDK failure.
- [ ] Implement `build.rs` validation of exact headers/import library/architecture and link only when `vmp-sdk` is enabled.
- [ ] Add minimal FFI for `VMProtectIsProtected`, `VMProtectIsDebuggerPresent`, `VMProtectIsVirtualMachinePresent`, `VMProtectIsValidImageCRC`, and marker begin/end functions. Keep all unsafe code inside `vmp.rs`.
- [ ] Wrap only login acceptance, heartbeat classification, operation admission, CRC dispatch and build identity in synchronous marker scopes; never wrap async/Tokio/Tauri/HTTP/device loops.
- [ ] Document the intended VMP modes (Ultra/Virtualization/Mutation), Memory Protection, Import Protection and Packing, with VM-denial disabled.
- [ ] Verify normal tests and an external-SDK `cargo check --features vmp-sdk`; commit `feat(protection): integrate external vmp sdk probe`.

### Task 5: 登录与心跳接入签名能力，token/password 零化

**Files:**
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/src/auth.rs`
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/src/api_client.rs`
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/Cargo.toml`
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/src/session_lifecycle.rs`
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/tests/session_lifecycle.rs`
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/Cargo.toml`
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/auth.rs`
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/session_capabilities.rs`
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/lib.rs`
- Modify: `src/Nwflash.Desktop/src/app/App.tsx`
- Modify: `src/Nwflash.Desktop/src/AppSessionAuthFlow.test.tsx`

**Interfaces:**
- Process identity `{ build_id, process_nonce }` generated in Rust once per process.
- Session capability holds verified claims and last heartbeat sequence, while bearer token is a non-`Debug`, zeroizing Rust secret.
- Heartbeat result distinguishes accepted lease, terminal server response and transient transport failure.

- [ ] Write RED tests for login rejecting unsigned/tampered/mismatched leases before capability activation, token clearing on replacement/logout, and password state clearing immediately after invoke starts.
- [ ] Write RED lifecycle tests for explicit terminal responses, invalid heartbeat signatures, sequence rollback and exactly three consecutive transient failures.
- [ ] Extend login request with process identity, verify the login lease before storing token/capability, and pass sequence into each heartbeat.
- [ ] Replace plain token storage with a zeroizing secret wrapper; expose token only for the shortest authenticated request scope and redact `Debug`/errors.
- [ ] Update the React submit path to copy the password into the invoke payload and synchronously clear component state before awaiting the response.
- [ ] Run infrastructure/application/Tauri/frontend targeted tests and commit `feat(session): enforce signed login and heartbeat leases`.

### Task 6: ExitPending 状态机、任务完成后退出与篡改立即退出

**Files:**
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/src/operation_coordinator.rs`
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/tests/operation_coordinator.rs`
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/exit_supervisor.rs`
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/integrity_reporter.rs`
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/lib.rs`
- Test: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/exit_supervisor.rs`

**Interfaces:**
- Operation admission gate states: `Running`, `ExitPending`, `Terminating`.
- `OperationCoordinator::wait_until_idle()` is notified when the active semaphore lease drops.
- `ExitSupervisor` receives normalized reason/phase, rejects new work, waits idle when required, flushes best-effort report/goodbye, clears session, then calls an injected `ProcessTerminator`.

- [ ] Write RED tests proving ExitPending rejects new operations, exits immediately when idle, waits while an operation is active, exits after that lease drops, and never cancels the active operation.
- [ ] Write RED tests proving local tamper reports with a 750 ms total deadline, never retries, omits token/path/serial/output and terminates regardless of report success/timeout.
- [ ] Add coordinator idle notification and a protection permission gate checked before every operation begins.
- [ ] Implement a Rust-owned supervisor task. Production terminator calls `std::process::exit`; tests inject a recording terminator so the test process survives.
- [ ] Route heartbeat terminal decisions to delayed exit and local CRC/lease/pin tamper decisions to immediate exit. Do not depend on a React listener.
- [ ] Revoke capability and zeroize token only after the current operation becomes idle; perform best-effort goodbye/report before final termination.
- [ ] Run application/Tauri tests and commit `feat(runtime): enforce protected exit policies`.

### Task 7: 在登录与高风险操作安全点执行独立完整性复核

**Files:**
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/lib.rs`
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/session_capabilities.rs`
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/src/operation_coordinator.rs`
- Test: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/lib.rs`
- Test: `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/tests/operation_coordinator.rs`

- [ ] Write RED tests for startup/login/session-restore/high-risk-operation CRC checks and stale/expired capability rejection at command admission.
- [ ] Add one process-scoped protection context to AppState and inject its admission gate into the existing coordinator, so every sensitive command receives the same check without per-command duplication.
- [ ] Recheck lease expiry, build/process nonce and heartbeat sequence at operation start, independently of React `has_token` and the initial login branch.
- [ ] Run CRC only at documented safe points; assert tests do not invoke it from operation progress/write loops.
- [ ] Run targeted tests and commit `feat(runtime): gate local operations with protection state`.

### Task 8: Tauri capability、release profile 与 VMP 手工交接加固

**Files:**
- Modify: `src/Nwflash.Desktop/src-tauri/Cargo.toml`
- Modify: `src/Nwflash.Desktop/src-tauri/capabilities/default.json`
- Create: `src/Nwflash.Desktop/src-tauri/capabilities/e2e.json`
- Modify: `src/Nwflash.Desktop/src/windowPermissions.test.ts`
- Inspect/Modify: existing protected release scripts under `scripts/` and `src/Nwflash.Desktop/scripts/`
- Create: `scripts/vmp/prepare-manual-handoff.ps1`
- Create: `scripts/vmp/accept-manual-output.ps1`
- Create: `scripts/vmp/protected-release-contract.ps1`
- Modify: release documentation associated with the existing Rust build.

- [ ] Write RED contract tests that production capability contains no WDIO/WebDriver permission/plugin, while the E2E configuration still supports native automation.
- [ ] Add protected Cargo profile settings: fat LTO, one codegen unit, panic abort, incremental off and symbols/PDB retained for the pre-VMP artifact.
- [ ] Make protected release require `NWFLASH_SESSION_VERIFY_KEY_B64`, `NWFLASH_BUILD_ID`, external VMP SDK and an explicit protected-output path.
- [ ] Implement the Lite GUI handoff: verify exact input EXE/PDB, record SHA-256, reject in-place output, require changed non-empty output, verify PE architecture and VMP protected/CRC probes, then allow signing/package stages.
- [ ] Ensure final package contract rejects the unprotected EXE, PDB/MAP, VMP SDK files, unexpected DLLs and invalid/absent Authenticode signatures.
- [ ] Run frontend capability tests and PowerShell script contract tests; commit `build(release): add vmp protected handoff`.

### Task 9: 全量验证与安全审计

**Files:**
- Modify only defects revealed by verification.
- Update: `docs/superpowers/specs/2026-08-23-vmp-network-integrity-hardening-design.md` only if implementation-compatible clarifications are required.

- [ ] Run `cargo fmt --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --check`.
- [ ] Run `cargo clippy --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --workspace --all-targets -- -D warnings`.
- [ ] Run `cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --workspace --no-fail-fast`.
- [ ] Run `npm --prefix src/Nwflash.Desktop test` and `npm --prefix src/Nwflash.Desktop run build`.
- [ ] Run `npm --prefix cloudflare test` and a non-deploying Wrangler type/build check.
- [ ] Run existing Tauri native E2E tests with the E2E-only capability configuration.
- [ ] Run protected-release preflight with the supplied x64 VMP SDK, then exercise the Lite GUI handoff verifier against a copied output artifact. Do not claim GUI protection if manual VMP processing has not actually occurred.
- [ ] Audit source/package outputs for private keys, bearer tokens, passwords, device serials, WDIO production permissions, `danger_accept_invalid_certs`, proxy acceptance, PDB/MAP and bundled VMP SDK files.
- [ ] Run `git diff --check`, inspect `git status --short`, and commit final fixes as `test(security): verify protected rust release`.

## External release prerequisites

Code completion and local tests do not authorize deployment or possession of secrets. Before a real protected production release, the release operator must:

1. Generate an Ed25519 key pair; store only PKCS#8 private key as Cloudflare Secret and compile the raw public key into the protected client.
2. Apply the D1 migration and deploy the Worker after its tests pass.
3. Run the pre-VMP protected Rust build with a unique build ID.
4. Open the supplied VMProtect Lite GUI, apply the documented function modes/options, save to a distinct output EXE and pass the handoff verifier.
5. Authenticode-sign the protected EXE, rebuild/sign NSIS, and run installer plus real login/heartbeat/device-operation smoke tests.
