# Current Device Target and Staging Cleanup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ensure every operation uses only the current unique device snapshot at the moment a command stage executes, while removing owned ROOT staging at successful completion and session stop and keeping public IPC free of raw serials and command arrays.

**Architecture:** Preparation may retain an internal transient command target so a plan can be built, but execution resolves the target again from `DeviceRuntime`; no serial is compared, accepted from the frontend, used as an identity, or persisted as a cross-step execution gate. Rust-owned ROOT patch workspaces are tracked by the runtime and removed only at explicit completion/session cleanup boundaries.

**Tech Stack:** Rust workspace (Tauri commands, nwflash-application services, Tokio), serde DTOs, Rust unit/integration tests, React/TypeScript frontend, Cargo and npm verification.

## Global Constraints

- Each application launch supports exactly one connected device; multiple discovered devices remain an explicit error.
- A serial may be derived from the current Rust device snapshot only as a transient ADB/Fastboot command argument.
- Never compare, cache, expose as frontend input, or use a serial as a preflight/execute identity binding.
- Only Rust-owned temporary ROOT staging paths may be removed; never delete a caller-supplied image or external directory.
- Do not modify `cloudflare/**` or unrelated existing migration changes.
- Follow test-driven development: each behavior change gets a failing regression test before production code.

### Task 1: Refresh Safe Flash execution targets

**Files:**
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/src/safe_flash.rs:91-280`
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/tests/safe_flash.rs:222-285` and all `SafeFlashExecutionRequest` literals
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/safe_flash.rs:624-657` and its unit-test request fixtures

**Interfaces:**
- `SafeFlashExecutionRequest<'a>` gains `serial: &'a str`; `SafeFlashExecutionService::execute` uses this transient field for the initial ADB/Fastboot command sequence.
- `execute_prepared_safe_flash` resolves `active_safe_flash_serial(&state)` immediately before creating the request. `SafeFlashBuildOptions.serial` remains preparation metadata only.

- [ ] **Step 1: Write the failing test**

Add a non-transition execution test with `SafeFlashBuildOptions.serial == "PRECHECK-STALE"` and `SafeFlashExecutionRequest.serial == "CURRENT-FASTBOOT"`; assert every emitted fastboot argument containing `-s` uses `CURRENT-FASTBOOT`.

- [ ] **Step 2: Run the focused test and verify it fails**

Run `cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml -p nwflash-application --test safe_flash execution_uses_current_execution_target -- --exact --nocapture`.
Expected result: compile failure because `SafeFlashExecutionRequest` has no transient `serial` field or assertion observes `PRECHECK-STALE`.

- [ ] **Step 3: Implement the minimal target refresh**

Add the field, replace `request.options.serial.clone()` with `request.serial.to_owned()`, resolve the current serial at the Tauri execution boundary, and update all request literals to pass the current target explicitly. Preserve the existing fastbootd rediscovery behavior.

- [ ] **Step 4: Run focused and application tests**

Run `cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml -p nwflash-application --test safe_flash` and confirm all tests pass.

### Task 2: Refresh ROOT multi-stage targets

**Files:**
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/root.rs:1470-1775`
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/root.rs` unit tests near automatic execution helpers
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/safe_flash.rs` automatic ROOT call site

**Interfaces:**
- Manager installation, patching, and final flashing each call the existing `DeviceRuntime` active snapshot resolver immediately before building their command/request.
- Automatic ROOT passes a fresh transient target into Safe Flash execution; no stage compares it to a prior target.

- [ ] **Step 1: Write failing ROOT regression tests**

Exercise the manager/patch/final command builders with a runtime snapshot changed between stages and assert the command for each stage contains the stage-current serial, never the first-stage serial. Add a test for the automatic Safe Flash request showing its target is resolved at execution.

- [ ] **Step 2: Run the focused ROOT tests and verify failure**

Run `cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml -p nwflash-tauri commands::root::tests -- --nocapture`.
Expected result: at least one assertion reports the stale first-stage serial.

- [ ] **Step 3: Implement per-stage resolution**

Move serial derivation into each stage's command-construction boundary, thread the transient value through helper arguments, and remove cross-stage serial reuse/comparison.

- [ ] **Step 4: Run ROOT and Safe Flash suites**

Run `cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml -p nwflash-tauri commands::root::tests commands::safe_flash::tests -- --nocapture` and confirm all pass.

### Task 3: Clean Rust-owned ROOT patch staging

**Files:**
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/root.rs:404-520,1620-1775`
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/session.rs:39-50`
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/root.rs` runtime cleanup tests

**Interfaces:**
- `RootPatchedArtifactRuntime` exposes an owned-staging cleanup operation that removes all tracked `staging_root` paths and clears artifact state.
- Successful patched-artifact flash consumes and then cleans the corresponding owned staging; `session_stop` invokes this cleanup alongside OTA runtime cleanup.

- [ ] **Step 1: Write failing cleanup tests**

Create a Rust-owned temporary staging directory, register an artifact, assert successful execution removes the directory, and assert `session_stop` removes every tracked staging directory for both artifact roles.

- [ ] **Step 2: Run the focused tests and verify failure**

Run `cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml -p nwflash-tauri root_patched_artifact_runtime -- --nocapture`.
Expected result: the directories remain because cleanup is currently limited to role replacement.

- [ ] **Step 3: Implement ownership-aware cleanup**

Centralize removal of tracked Rust-owned paths, call it after a successful flash and from `session_stop`, preserve retry semantics on failures/cancellation, and leave external user paths untouched.

- [ ] **Step 4: Run cleanup and session tests**

Run `cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml -p nwflash-tauri commands::root::tests commands::session::tests -- --nocapture`.

### Task 4: Restrict Quick Flash public IPC

**Files:**
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/quick_flash.rs:45-70,932-948`
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/lib.rs:480-490`
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/quick_flash.rs` tests around prepare handlers

**Interfaces:**
- Public prepare handlers return a safe summary DTO containing operation kind, transport, partition/image labels, and command count only; they do not serialize serials, executable arguments, or raw command arrays.
- Internal command builders remain available to Rust tests and execution paths.

- [ ] **Step 1: Write the failing IPC boundary test**

Serialize the public prepare response and assert it contains no `serial`, `program`, `args`, or command-array fields while preserving the safe summary fields.

- [ ] **Step 2: Run the focused Quick Flash tests and verify failure**

Run `cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml -p nwflash-tauri commands::quick_flash::tests -- --nocapture`.
Expected result: current DTO serialization includes `serial` and raw command arguments.

- [ ] **Step 3: Implement the safe summary DTO and registration boundary**

Replace the public DTO fields and update the handler/registration; retain internal plan/command tests without exposing them through Tauri IPC.

- [ ] **Step 4: Run Quick Flash and frontend contract tests**

Run `cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml -p nwflash-tauri commands::quick_flash::tests -- --nocapture` and `npm test --prefix src/Nwflash.Desktop -- --runInBand`.

### Task 5: Document, clean, and verify the release

**Files:**
- Modify: `docs/project-architecture.md`, `docs/architecture.md`, `docs/architecture-tauri-migration.md`, `docs/safeflash-ota.md`, `src/Nwflash.Desktop/README.md`
- Modify: any focused test fixtures required by Tasks 1-4

- [ ] **Step 1: Update architecture wording**

State that serial may be displayed read-only in device status, but it cannot be supplied by the frontend or used as identity/binding, cache key, cross-step comparison, or execution gate.

- [ ] **Step 2: Inspect and remove only this run's temporary files**

Use `git status --short` and timestamps/known paths; remove only temporary files created by this implementation, never `node_modules`, `dist`, `target`, existing logs, `.git/objects`, or user-owned firmware/images.

- [ ] **Step 3: Run complete verification**

Run:

```powershell
cargo fmt --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --all -- --check
cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --workspace --all-targets
cargo clippy --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --workspace --all-targets --all-features -- -D warnings
npm test --prefix src/Nwflash.Desktop
npx --prefix src/Nwflash.Desktop tsc --noEmit
npm run build --prefix src/Nwflash.Desktop
npm run tauri --prefix src/Nwflash.Desktop -- build --no-bundle
```

- [ ] **Step 4: Record the rebuilt executable hash**

Compute `Get-FileHash src/Nwflash.Desktop/src-tauri/target/release/nwflash-desktop.exe -Algorithm SHA256` after the final build and report the resulting size/hash, not the pre-fix artifact.
