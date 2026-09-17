# Session Capability Revocation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make all Rust-resident destructive capabilities session-bound, revoke them atomically at logout/session stop, and prevent stale asynchronous work from republishing them.

**Architecture:** A Tauri `SessionCapabilityScope` provides epoch leases and a mutex-protected commit/invalidate boundary. An application-layer idle lease owns the same semaphore used by operations, so teardown cannot race an authorization/admission window. Runtimes store the epoch with their state; producers commit only while their captured lease is current and consumers reject a stale epoch.

**Tech Stack:** Rust 2021, Tokio semaphore/mutex, Tauri state, existing command-module unit tests.

## Global Constraints

- Do not cancel an active flash or delete its staging during logout/session stop; return `OPERATION_IN_PROGRESS_MESSAGE` while the idle lease cannot be acquired.
- Preserve user-chosen local image files; delete only explicitly owned staging roots.
- Epoch invalidation and runtime clearing share one commit barrier, so late work can neither retain nor consume prior-session state.
- Every producer captures its epoch before dialog/network/operation work and commits with that lease; every opaque-ID/plan consumer checks its stored epoch.
- Keep browser DTOs free of epochs, paths, hashes, staging roots, and device secrets.
- Preserve unrelated dirty/untracked migration work; do not stage/commit product files without explicit ownership authorization.
- Follow TDD for every behavior change and request a scoped review after each task.

---

## File Structure

| File | Responsibility |
| --- | --- |
| `crates/nwflash-application/src/operation_coordinator.rs` | Atomic idle lease over the operation semaphore. |
| `crates/nwflash-application/tests/operation_coordinator.rs` | Admission/idle-lease race regressions. |
| `crates/nwflash-tauri/src/session_capabilities.rs` | Epoch lease capture, commit, current-check, activation, invalidation. |
| `crates/nwflash-tauri/src/lib.rs` | Scope ownership and centralized runtime teardown. |
| `crates/nwflash-tauri/src/commands/{auth,session}.rs` | Login activation and safe logout/session-stop lifecycle. |
| `crates/nwflash-tauri/src/commands/{root,root_ota}.rs` | ROOT image/artifact/OTA epoch tagging, checks, cleanup. |
| `crates/nwflash-tauri/src/commands/{safe_flash,quick_flash}.rs` | Safe Flash and Quick Flash prepared capability epoch tagging, cleanup. |
| `crates/nwflash-tauri/src/commands/firmware.rs` | Firmware artifact/extraction/inspection epoch tagging, cleanup. |

### Task 1: Add Atomic Operation Idle Lease and Epoch Scope

**Files:**
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/src/operation_coordinator.rs`
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/tests/operation_coordinator.rs`
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/session_capabilities.rs`
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/lib.rs`

**Interfaces:**
- Produces `OperationCoordinator::try_acquire_idle() -> Result<OperationIdleLease, OperationCoordinatorError>` using an owned permit.
- Produces `SessionCapabilityLease { epoch: u64 }` and `SessionCapabilityScope::{activate,capture,commit,is_current,invalidate}`.

- [ ] **Step 1: Write failing core concurrency tests**

Add an operation-coordinator test that blocks authorization after `run_async` owns its permit, then asserts `try_acquire_idle()` returns `InProgress`. Add a scope test:

```rust
let scope = SessionCapabilityScope::new();
let first = scope.activate();
scope.invalidate(|| {});
let second = scope.activate();
assert_ne!(first, second);
assert!(scope.commit(first, || ()).is_err());
assert!(scope.commit(second, || ()).is_ok());
```

Add a lock-barrier test where invalidation happens before a delayed producer calls `commit`; assert publish closure never runs.

- [ ] **Step 2: Run RED**

```powershell
cargo test -p nwflash-application --test operation_coordinator
cargo test -p nwflash-tauri session_capabilities
```

Expected: idle lease and session capability scope symbols do not exist.

- [ ] **Step 3: Implement the core primitives**

Acquire an `OwnedSemaphorePermit` with `try_acquire_owned` for `OperationIdleLease`; it releases on drop. Implement scope state as `{ active: bool, epoch: u64 }` behind one `std::sync::Mutex`. `commit` holds the scope lock while confirming `active && epoch == lease.epoch` and running only the short runtime-state publication closure. `invalidate` increments epoch, marks inactive, and invokes only in-memory clear/owned-root collection work under the same barrier; filesystem deletion runs after releasing it.

- [ ] **Step 4: Run GREEN**

```powershell
cargo test -p nwflash-application --test operation_coordinator
cargo test -p nwflash-tauri session_capabilities
```

- [ ] **Step 5: Record task result without staging user-owned product files**

Do not stage/commit existing migration source; write task report and proceed to review.

### Task 2: Epoch-Bind and Clear ROOT/ROOT OTA Capabilities

**Files:**
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/root.rs`
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/root_ota.rs`
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/lib.rs`

**Interfaces:**
- Consumes Task 1 `SessionCapabilityLease` and scope commit/current checks.
- Produces runtime `clear_owned()` methods returning only owned roots for caller deletion; ROOT image external files are never returned.

- [ ] **Step 1: Write stale ROOT and late ROOT OTA regressions**

Use a scope lease to publish a verified ROOT image/artifact, invalidate, then assert lookup/prepared-plan consumption fails. For delayed `root_ota_check`, capture a lease, invalidate before its simulated publish, and assert no resolved OTA remains. Assert an owned artifact/OTA staging root is collected while an external selected image remains.

- [ ] **Step 2: Run RED**

```powershell
cargo test -p nwflash-tauri commands::root::tests
cargo test -p nwflash-tauri commands::root_ota::tests
```

Expected: runtimes have no epoch field/clear API and stale work can publish/resolve.

- [ ] **Step 3: Implement ROOT epoch and ownership boundaries**

Add epoch to `RootImageSelection`, `RootPatchedArtifact`, `ResolvedRootOta`, and relevant prepared ROOT plan state. Capture once before any dialog/network/patch work; publish via `scope.commit`; verify current epoch before every get/take/resolve. Add clear methods that atomically remove state/collect only owned roots, then delete roots after scope/runtime locks. On a rejected late commit, delete only its newly-created owned staging.

- [ ] **Step 4: Run GREEN**

```powershell
cargo test -p nwflash-tauri commands::root::tests
cargo test -p nwflash-tauri commands::root_ota::tests
```

### Task 3: Epoch-Bind and Clear Safe Flash/Quick Flash Preflights

**Files:**
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/safe_flash.rs`
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/quick_flash.rs`
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/lib.rs`

**Interfaces:**
- Consumes Task 1 scope/lease.
- Produces epoch-tagged pending Safe Flash, firmware-artifact, and dual-slot plans; clear APIs that never delete browser/local image paths.

- [ ] **Step 1: Write stale preflight and late publish regressions**

Prepare each runtime with an old epoch, invalidate/re-activate, and assert execute/take rejects it. Simulate Safe Flash/dual-slot publication after invalidation and assert the scope rejects it; candidate `source.staging_root` is returned for cleanup while `partitions[*].image_path` is untouched.

- [ ] **Step 2: Run RED**

```powershell
cargo test -p nwflash-tauri commands::safe_flash::tests
cargo test -p nwflash-tauri commands::quick_flash::tests
```

- [ ] **Step 3: Implement epoch fields, commit and teardown**

Capture a lease before each preflight work path. Store epoch with `PreparedSafeFlashSession`, `PreparedFirmwareArtifactRuntime`, and `PreparedDualSlotRuntime`; check it before begin/take. Add clear-pending methods that reject executing Safe Flash; central teardown only runs while Task 1 idle lease is held, so the execution case cannot occur. Use scope commit for post-`run_async` Safe Flash/dual-slot publication and clean rejected late candidate staging.

- [ ] **Step 4: Run GREEN**

```powershell
cargo test -p nwflash-tauri commands::safe_flash::tests
cargo test -p nwflash-tauri commands::quick_flash::tests
```

### Task 4: Epoch-Bind and Clear Firmware Capabilities

**Files:**
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/firmware.rs`
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/lib.rs`

**Interfaces:**
- Consumes Task 1 scope/lease.
- Produces epoch-tagged firmware artifacts/extraction/payload/remote inspections and owned-root collection clear methods.

- [ ] **Step 1: Write stale firmware and late extraction regressions**

Create a firmware artifact/result under an old lease, invalidate, and assert opaque IDs reject. Simulate extraction completing after invalidation; assert result snapshot root is deleted and no result ID remains. Cover inspection selection IDs similarly.

- [ ] **Step 2: Run RED**

```powershell
cargo test -p nwflash-tauri commands::firmware::tests
```

- [ ] **Step 3: Implement epoch-aware stores and cleanup**

Store epoch alongside each runtime value/store. Capture before inspection/extraction/artifact work; publish only through scope commit. Make clear methods take current state, invalidate IDs, and collect/drop only internal staging roots outside locks. Consumers compare stored epoch to current lease before resolving IDs.

- [ ] **Step 4: Run GREEN**

```powershell
cargo test -p nwflash-tauri commands::firmware::tests
```

### Task 5: Integrate Login, Session Stop, Logout, and Central Teardown

**Files:**
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/lib.rs`
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/auth.rs`
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/session.rs`
- Test: command inline tests and `src/Nwflash.Desktop/src/AppSessionLifecycle.test.tsx` only if command contract changes reach UI

**Interfaces:**
- Consumes Tasks 1–4.
- Produces one `AppState::invalidate_session_capabilities` path used by logout and session stop.

- [ ] **Step 1: Write failing lifecycle regressions**

Test that logout/session stop cannot acquire the idle lease during a pending authorization/operation and return `OPERATION_IN_PROGRESS_MESSAGE` without clearing token/UI state. Test successful teardown invalidates one capability from every runtime and deletes only owned roots. Test login activates a fresh epoch and old leases cannot consume after relogin.

- [ ] **Step 2: Run RED**

```powershell
cargo test -p nwflash-tauri commands::auth::tests
cargo test -p nwflash-tauri commands::session::tests
```

- [ ] **Step 3: Implement centralized lifecycle handling**

Make `auth_logout` async. Both handlers acquire the idle lease first, invalidate scope and clear runtime state through one AppState method, then perform lifecycle stop/token removal/usage flush. Treat an already-stopped lifecycle as teardown-safe; never leave a valid token plus prior-session capability. Keep frontend behavior unchanged except surfacing the existing in-progress message when teardown is refused.

- [ ] **Step 4: Run GREEN**

```powershell
cargo test -p nwflash-tauri commands::auth::tests
cargo test -p nwflash-tauri commands::session::tests
```

## Final Verification

- [ ] Run `cargo fmt --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --check`.
- [ ] Run `cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --workspace --no-fail-fast`.
- [ ] Run `cargo clippy --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --workspace --all-targets -- -D warnings`.
- [ ] Run a whole-plan security review focused on epoch/idle linearization, late publication cleanup, and external-file preservation.
