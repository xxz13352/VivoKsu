# Runtime Hash and Serial Binding Removal Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove runtime hash gates and cross-step phone serial bindings while retaining immediate transport targeting, runtime ownership, session epoch lifetime control, and release/resource integrity.

**Architecture:** Runtime records become content- and device-neutral: opaque IDs, session epoch, image metadata, partition metadata, and owned staging remain. Command handlers derive a fresh unique ADB/Fastboot target immediately before building each command group. OTA transfer keeps response-length/disk/staging safeguards but no catalog SHA-256 gate.

**Tech Stack:** Rust 2021, Tauri 2, Tokio, existing Cargo unit/integration tests.

## Global Constraints

- Do not remove SHA-256/manifest checks for platform tools, release packages, installer signing, scrcpy, payload_dumper, manager APKs, or other provisioned external resources.
- Do not remove current-device discovery, multiple-device rejection, or the `-s <serial>` parameter used to target an immediate ADB/Fastboot command.
- Do remove runtime image/artifact/OTA hashes and every serial comparison that rejects work because a preflight/cached serial differs from the current device.
- Session capability epoch is session-lifetime control, not phone serial binding; retain it.
- Preserve path/format/non-empty/partition/capacity/staging/cancellation/first-failure behavior.
- Use TDD and preserve unrelated dirty/untracked user work; do not stage/commit product source.

---

## File Structure

| File | Responsibility |
| --- | --- |
| `crates/nwflash-tauri/src/commands/root.rs` | Remove ROOT image/artifact hashes and device-serial gate state. |
| `crates/nwflash-tauri/src/commands/root_ota.rs` | Remove cached/post-extraction serial gate state. |
| `crates/nwflash-application/src/safe_flash.rs` | Remove preflight serial equality and accept a sole current fastboot target. |
| `crates/nwflash-tauri/src/commands/safe_flash.rs` | Remove Safe Flash prepared-device serial rejection and runtime OTA hash mapping. |
| `crates/nwflash-infrastructure/src/ota_download.rs` | Remove runtime SHA descriptor/hash computation while retaining bounded transfers. |
| `crates/nwflash-application/src/quick_flash.rs` | Keep plan serial only as immediate command input, not validation identity. |
| `crates/nwflash-tauri/src/commands/quick_flash.rs` | Re-resolve serial immediately before execution and remove prepared/preview binding comparisons. |
| current architecture docs | Describe the product decision and remove claims that runtime gates are active requirements. |

### Task 1: Remove ROOT/ROOT OTA Hash and Serial Gates

**Files:**
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/root.rs`
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/root_ota.rs`

**Interfaces:**
- Consumes current session epoch scope and opaque runtime IDs.
- Produces target-neutral ROOT selections/artifacts/OTA records; immediate patch commands still resolve their current ADB/Fastboot target.

- [ ] **Step 1: Replace rejection tests with target-neutral behavior tests**

Write tests that overwrite a selected/root-patched file with same-size different bytes and confirm lookup/prepared-plan consumption still succeeds. Replace serial-mismatch tests with assertions that a ROOT image/artifact/OTA record contains no serial and can be used after the current device target changes. Preserve stale epoch and opaque-ID rejection tests.

- [ ] **Step 2: Run RED**

```powershell
cargo test -p nwflash-tauri commands::root::tests
cargo test -p nwflash-tauri commands::root_ota::tests
```

Expected: current content-change and serial-mismatch gates reject these new target-neutral expectations.

- [ ] **Step 3: Remove runtime hash and serial state/checks**

Delete ROOT selection/artifact fingerprint fields and SHA helpers, remove `device_serial` fields and `verify_*device_binding` functions, and simplify lookup/take APIs to require only current epoch plus opaque ID. Patch validation continues to enforce existing output validity but returns only `FlashImageInfo`; `publish_root_patch_candidate` accepts that result without a digest. Remove cached/root-OTA serial comparisons and post-extraction fresh-serial rejection. Retain current serial only where an immediate ADB/Fastboot command is built.

- [ ] **Step 4: Run GREEN**

```powershell
cargo test -p nwflash-tauri commands::root::tests
cargo test -p nwflash-tauri commands::root_ota::tests
```

### Task 2: Remove Online OTA SHA Gate and Safe Flash Serial Binding

**Files:**
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/src/ota_download.rs`
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/src/lib.rs`
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/src/safe_flash.rs`
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/tests/safe_flash.rs`
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/tests/ota_download.rs`
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/safe_flash.rs`

**Interfaces:**
- Consumes ordinary `download_to_file_with_cancellation` response-length/staging behavior.
- Produces Safe Flash execution that targets the sole current fastboot device and does not compare it with preflight serial.

- [ ] **Step 1: Write target-neutral/without-SHA regressions**

Replace wrong-SHA rejection with an equal-length altered OTA fixture that completes preparation. Replace prepared-device mismatch and changed-fastbootd serial rejection with a sole current device target that succeeds. Keep multiple-device and `is-userspace` rejection tests.

- [ ] **Step 2: Run RED**

```powershell
cargo test -p nwflash-infrastructure --test ota_download
cargo test -p nwflash-application --test safe_flash
cargo test -p nwflash-tauri commands::safe_flash::tests
```

Expected: SHA APIs/handlers and serial equality checks reject the new behavior.

- [ ] **Step 3: Remove SHA descriptor and serial mismatch logic**

Remove `OtaExpectedIntegrity`, SHA parsing/computation/comparison, verified-download exports, and Safe Flash catalog hash mapping. Keep HEAD/Range length, byte ceiling, disk capacity, cancellation, staging, and publish cleanup. Remove `SafeFlashBuildOptions.serial == execution serial` checks; resolve the sole current fastboot target in the execution path, and let fastbootd waiting select the sole discovered device instead of matching an earlier ADB serial.

- [ ] **Step 4: Run GREEN**

```powershell
cargo test -p nwflash-infrastructure --test ota_download
cargo test -p nwflash-application --test safe_flash
cargo test -p nwflash-tauri commands::safe_flash::tests
```

### Task 3: Re-resolve Quick Flash Transport and Update Current Docs

**Files:**
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/src/quick_flash.rs`
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/quick_flash.rs`
- Modify: relevant Quick Flash/domain tests
- Modify: `docs/index.md`, `docs/architecture.md`, `docs/project-architecture.md`, `docs/architecture-tauri-migration.md`, `src/Nwflash.Desktop/docs/rust-tauri-architecture.md`

**Interfaces:**
- Consumes a stored plan only for operation/partition/image metadata.
- Produces commands using a freshly resolved unique transport serial at execution, including slot switch/reboot.

- [ ] **Step 1: Write fresh-target execution regressions**

Build/prep a plan on `SERIAL-A`, arrange the runtime to expose the sole `SERIAL-B`, and assert execution uses `SERIAL-B` rather than rejecting. Add an ADB-to-fastbootd test where the discovered sole fastboot serial differs. Keep multiple-device rejection.

- [ ] **Step 2: Run RED**

```powershell
cargo test -p nwflash-application --test quick_flash
cargo test -p nwflash-tauri commands::quick_flash::tests
```

Expected: `verify_execution_device` and expected-serial fastbootd waiting reject/change target incorrectly.

- [ ] **Step 3: Derive serial at execution boundaries and revise docs**

Replace plan/current serial equality checks with a helper that resolves the current unique transport serial immediately before command construction, overwrites the transient command-plan serial, and uses it for slot-switch/reboot. Remove serial-bound capability claims and hash-active claims from current architecture documentation; link the product decision. Do not change historical spec/plan records beyond an explicit superseded note if needed.

- [ ] **Step 4: Run GREEN**

```powershell
cargo test -p nwflash-application --test quick_flash
cargo test -p nwflash-tauri commands::quick_flash::tests
```

## Final Verification

- [ ] Run `cargo fmt --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --check`.
- [ ] Run `cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --workspace --no-fail-fast`.
- [ ] Run `cargo clippy --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --workspace --all-targets -- -D warnings`.
- [ ] Run an independent review confirming no runtime SHA/fingerprint or cross-step phone serial gate remains, while resource/release integrity and immediate command targeting stay intact.
