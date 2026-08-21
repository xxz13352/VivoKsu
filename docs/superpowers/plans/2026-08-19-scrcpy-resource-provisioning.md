# scrcpy Resource Provisioning Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Keep scrcpy out of user-selected file workflows while making bundled-resource detection, resource-install downloads, and installed-package integrity checks reliable.

**Architecture:** `ScrcpyProvisioner` resolves scrcpy internally. It first checks a trusted bundle location, then a verified installed package, then downloads the release asset selected from validated GitHub metadata through the existing mirror-aware downloader. The existing `resource_install(["scrcpy"])` command remains the only user-triggered installation action; no frontend path input is added.

**Tech Stack:** Rust 2021, Tauri 2, reqwest, sha2, serde, Tokio, React/Vitest.

## Global Constraints

- Do not add a scrcpy file picker, path input, or arbitrary executable IPC argument.
- Do not modify `cloudflare/**`.
- Preserve unrelated dirty-worktree changes.
- Use a focused failing regression test before each production behavior change.
- Release asset archives must retain SHA-256 and declared-length verification.
- Do not commit changes unless explicitly requested.

---

### Task 1: Harden scrcpy source resolution and publication

**Files:**
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/src/scrcpy_provisioner.rs`
- Test: `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/src/scrcpy_provisioner.rs`
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/src/paths.rs` only if a shared bundled-resource helper is required

**Interfaces:**
- Consumes: `RemoteAssetSpec`, `RemoteAssetDownloader`, the current executable's `resources` directory, and `CancellationToken`.
- Produces: `ScrcpyProvisioner::new`, `is_installed`, `installed_executable`, and `ensure_installed` that accept only internally resolved resources.

- [x] **Step 1: Add failing tests for unsafe metadata and invalid installed packages**

Add tests that assert:

```rust
assert!(package_name_from_asset("..\\scrcpy.zip").is_err());
assert!(package_name_from_asset("C:\\scrcpy.zip").is_err());
assert!(verify_published_package(&package_root).is_err());
```

The package test must create a non-empty `scrcpy.exe`, omit or corrupt its integrity metadata, and prove the package is not considered installed.

- [x] **Step 2: Run the focused tests and verify they fail for the missing behavior**

Run:

```powershell
cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml -p nwflash-infrastructure scrcpy --quiet
```

Expected: the new assertions fail because asset names are used without validation and installed executables are currently accepted based only on non-zero length.

- [x] **Step 3: Implement the minimal internal provisioning changes**

Implement the following behavior in `scrcpy_provisioner.rs`:

- Resolve an optional bundled root at `<current executable>/resources/scrcpy` and use it only when it contains a valid `scrcpy.exe`.
- Validate the selected GitHub asset name as a single safe filename before creating `staging_root`, `archive_path`, and `package_root`; reject separators, absolute paths, `..`, and empty/non-UTF-8 names.
- Preserve the existing archive SHA-256 and expected-length checks.
- When publishing, compute the extracted `scrcpy.exe` SHA-256 and write a provisioner-owned manifest beside the package executable.
- Make installed-package discovery require a non-empty executable plus a valid manifest hash; invalid packages are ignored so the resource installer can replace them.
- Publish into a fresh package directory only after extraction succeeds, then remove only the owned staging directory.

- [x] **Step 4: Run focused tests and inspect the source boundary**

Run the focused Rust test command again and verify that all scrcpy tests pass. Also run:

```powershell
rg -n "scrcpy.*path|file.*picker|select.*scrcpy|scrcpy.*input" src/Nwflash.Desktop/src src/Nwflash.Desktop/src-tauri -g '!node_modules' -g '!dist'
```

Expected: no new frontend path-selection control or public arbitrary scrcpy path appears.

### Task 2: Verify the existing resource-installation UX remains the only trigger

**Files:**
- Test: `src/Nwflash.Desktop/src/pages/ResourceDownloadPage.test.tsx`
- Test: `src/Nwflash.Desktop/src/pages/SoftwarePage.test.tsx` if its resource readiness contract needs coverage
- Modify: `docs/architecture-tauri-migration.md`

**Interfaces:**
- Consumes: the existing `resource_inventory` and `resource_install` IPC commands.
- Produces: a frontend contract proving `scrcpy` is an installation item and no local path is requested.

- [x] **Step 1: Add a failing frontend contract test**

Render the resource page with an inventory containing `scrcpy`, assert that the item is shown as an installable resource, and assert that no file input or scrcpy path picker is rendered. Assert that clicking install invokes `resource_install` with `['scrcpy']`.

- [x] **Step 2: Run the focused frontend test and verify it fails if the contract is absent**

Run:

```powershell
npm --prefix src/Nwflash.Desktop run test -- src/pages/ResourceDownloadPage.test.tsx
```

- [x] **Step 3: Keep the existing resource installer boundary and update architecture documentation**

Do not add a new scrcpy UI command. Update `docs/architecture-tauri-migration.md` to state that scrcpy is resolved by Rust from bundled resources or the resource installer, and that arbitrary local scrcpy paths are not accepted over IPC.

- [x] **Step 4: Run the focused frontend test and the existing firmware-page contract**

Run:

```powershell
npm --prefix src/Nwflash.Desktop run test -- src/pages/ResourceDownloadPage.test.tsx src/pages/FirmwareExtractPage.test.tsx
```

Expected: PASS, with no scrcpy file-selection UI.

### Task 3: Run the verification gates

**Files:**
- No production files; inspect the final diff and generated artifacts only.

**Interfaces:**
- Consumes: the completed Rust provisioner and frontend resource contract.
- Produces: fresh evidence for focused tests, workspace tests, formatting, Clippy, and frontend build/tests.

- [x] **Step 1: Run focused Rust and frontend tests**

Run the commands from Tasks 1 and 2 and retain their exit codes and failure counts.

- [x] **Step 2: Run the Rust workspace test and formatting gates**

Run:

```powershell
cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --workspace --quiet
cargo fmt --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --all -- --check
cargo clippy --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --workspace --all-targets -- -D warnings
```

The workspace formatting check still reports two pre-existing line-wrap differences in `crates/nwflash-tauri/src/commands/device_identity.rs`; the scrcpy and mirror files pass standalone `rustfmt --check`. That unrelated dirty-worktree file was left unchanged.

- [x] **Step 3: Run the frontend test and build gates**

Run:

```powershell
npm --prefix src/Nwflash.Desktop run test -- --run
npm --prefix src/Nwflash.Desktop run build
```

- [x] **Step 4: Review the diff for scope and report any unrelated failures**

Run `git diff --check` and inspect `git diff --stat`. Do not revert unrelated user changes or claim a gate passed if the command output does not show success.
