# Rust Remediation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Repair the confirmed Rust/Tauri audit defects while preserving the single-device operating model.

**Architecture:** Remove unused raw IPC capabilities, keep secrets and destructive plans inside Rust-owned runtimes, verify resource identity before publication, and drain child process output concurrently. ROOT selections receive a content fingerprint that must match at each later workflow boundary.

**Tech Stack:** Rust 2021, Tauri 2, Tokio, reqwest, sha2, React/Vitest.

## Global Constraints

- Do not modify `cloudflare/**`.
- Do not introduce serial binding in any feature or change the one-device-per-launch model. Derive serial as a transient Rust command argument from the current unique device snapshot; an in-memory command plan may carry that target, but never use it as a cross-step preflight/execution gate.
- Preserve existing user changes in the dirty worktree.
- Every behavior change starts with a focused failing regression test, followed by the minimal implementation and a passing rerun.
- Update `docs/architecture-tauri-migration.md` after code changes so its command and resource boundaries match source.
- Run Rust workspace tests, `cargo fmt --check`, and strict Clippy before completion.

---

### Task 1: Remove Raw IPC Capabilities and Token Exposure

**Files:**
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/auth.rs`
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/firmware.rs`
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/quick_flash.rs`
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/lib.rs`
- Modify: `src/Nwflash.Desktop/src/app/ipc-events.ts`
- Modify: `src/Nwflash.Desktop/src/pages/FirmwareExtractPage.tsx`
- Test: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/auth.rs`
- Test: `src/Nwflash.Desktop/src/pages/FirmwareExtractPage.test.tsx`

**Interfaces:**
- Consumes: authenticated `AppState.session_token` and internal quick-flash helpers.
- Produces: `AuthSessionDto { username, name }`; no public raw-plan, ROM-resolve, or arbitrary-URL firmware command.

- [ ] **Step 1: Write a failing serialization test**

```rust
#[test]
fn auth_session_dto_never_serializes_a_bearer_token() {
    let dto = AuthSessionDto { username: "user".into(), name: "User".into() };
    let json = serde_json::to_string(&dto).unwrap();
    assert!(!json.contains("token"));
}
```

- [ ] **Step 2: Run the test red**

Run: `cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml -p nwflash-tauri auth_session_dto_never_serializes_a_bearer_token`

Expected: FAIL because `AuthSessionDto` still requires and serializes `token`.

- [ ] **Step 3: Write a failing remote-firmware UI regression test**

Add a page test that renders the firmware extraction workflow and asserts that it does not expose an arbitrary remote URL input or invoke `firmware_inspect_remote_payload`. Run that single test before changing the page; it must fail because the old remote-payload control is still rendered.

- [ ] **Step 4: Implement the minimal command-boundary changes**

Remove `token` from `AuthSessionDto` and its TypeScript DTO. Remove `#[tauri::command]` plus handler registration for `quick_flash_prepare_commands` and `quick_flash_execute_commands`, retaining their Rust-internal calls that construct constrained plans. Delete the unused generic `firmware_resolve` / `firmware_inspect_remote_payload` functions and their ROM-resolution DTO conversion rather than leaving unreachable raw capabilities. Remove the matching arbitrary-URL remote firmware workflow from `FirmwareExtractPage` and its obsolete test contract; preserve local firmware inspection and extraction.

- [ ] **Step 5: Run focused and frontend tests**

Run: `cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml -p nwflash-tauri auth_session_dto_never_serializes_a_bearer_token`

Run: `npm --prefix src/Nwflash.Desktop run test -- AppSessionAuthFlow.test.tsx AppSessionLifecycle.test.tsx src/pages/FirmwareExtractPage.test.tsx`

Expected: PASS.

### Task 2: Publish and Verify scrcpy Safely

**Files:**
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/src/scrcpy_provisioner.rs`
- Test: `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/src/scrcpy_provisioner.rs`

**Interfaces:**
- Consumes: official GitHub release asset `name`, `browser_download_url`, `digest`, and `size`.
- Produces: a `RemoteAssetSpec` with required SHA-256 and expected length, then an installed executable path.

- [ ] **Step 1: Write failing unit tests**

```rust
#[test]
fn release_asset_without_a_sha256_digest_is_rejected() {
    let asset = GitHubReleaseAsset {
        name: "scrcpy-win64-v4.1.zip".to_string(),
        browser_download_url: "https://github.com/Genymobile/scrcpy/releases/download/v4.1/scrcpy-win64-v4.1.zip".to_string(),
        digest: None,
        size: 11_305_298,
    };

    assert!(asset_spec(&asset).is_err());
}

#[test]
fn published_scrcpy_payload_survives_staging_cleanup() {
    let root = unique_test_directory("scrcpy-publication");
    let staging = root.join(".staging-test");
    let payload = staging.join("payload");
    let package = root.join("scrcpy-win64-v4.1");
    fs::create_dir_all(&payload).unwrap();
    fs::write(payload.join("scrcpy.exe"), b"scrcpy fixture").unwrap();

    publish_payload(&payload, &package).unwrap();
    fs::remove_dir_all(&staging).unwrap();

    assert_eq!(fs::read(package.join("scrcpy.exe")).unwrap(), b"scrcpy fixture");
    fs::remove_dir_all(root).unwrap();
}
```

- [ ] **Step 2: Run tests red**

Run: `cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml -p nwflash-infrastructure scrcpy`

Expected: FAIL because the current asset model has no digest and cleanup precedes copying.

- [ ] **Step 3: Implement minimal verified publication**

Extend the release asset model, require a well-formed `sha256:` digest, set SHA-256 and size in `RemoteAssetSpec`, fetch release metadata directly from GitHub, copy payload before deleting staging, and clean staging after success or failure.

- [ ] **Step 4: Run focused test green**

Run: `cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml -p nwflash-infrastructure scrcpy`

Expected: PASS.

### Task 3: Recover Corrupt Downloaded Resources

**Files:**
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/src/root_resources.rs`
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/src/payload_provisioner.rs`
- Test: corresponding `#[cfg(test)]` modules or `crates/nwflash-infrastructure/tests/` tests

**Interfaces:**
- Consumes: bundled and cache paths plus existing SHA-256 validators.
- Produces: verified resource paths only; corrupt cache files are replaced by verified downloads.

- [ ] **Step 1: Write failing selection and recovery tests**

```rust
#[test]
fn manager_selection_prefers_a_verified_cache_over_an_invalid_bundle() {
    let root = unique_test_directory("root-manager-selection");
    let bundled = root.join("apk").join("manager.apk");
    let cached = root.join("cache").join("manager.apk");
    write_invalid_apk_fixture(&bundled);
    write_valid_apk_fixture(&cached);
    let expected_hash = compute_sha256(&cached).unwrap();

    let selected = select_verified_manager_apk(&bundled, &cached, |path| {
        verify_apk_fixture(path, &expected_hash)
    })
    .unwrap();

    assert_eq!(selected, cached);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn corrupt_cached_payload_is_removed_before_a_reinstall_attempt() {
    let root = unique_test_directory("payload-cache-recovery");
    let cached = root.join(PAYLOAD_DUMPER_EXECUTABLE_NAME);
    fs::create_dir_all(&root).unwrap();
    fs::write(&cached, b"corrupt payload cache").unwrap();

    assert!(discard_invalid_cached_executable(&cached).unwrap());
    assert!(!cached.exists());
    fs::remove_dir_all(root).unwrap();
}
```

`write_valid_apk_fixture` must create a ZIP containing `AndroidManifest.xml`; `verify_apk_fixture` must use the same non-empty, SHA-256, and archive-manifest checks as the production candidate validator. `discard_invalid_cached_executable` must return `true` only when the pre-existing cache fails the production payload hash validator and is actually removed.

- [ ] **Step 2: Run tests red**

Run: `cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml -p nwflash-infrastructure root_resources payload_provisioner`

Expected: FAIL because non-empty files are currently treated as usable.

- [ ] **Step 3: Implement verified candidate selection**

Use existing APK/executable validators to select candidates. Ensure availability checks verify content, and remove an invalid cached payload before download.

- [ ] **Step 4: Run focused test green**

Run: `cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml -p nwflash-infrastructure root_resources payload_provisioner`

Expected: PASS.

### Task 4: Drain Process Pipes While Polling

**Files:**
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-windows/src/process.rs`
- Test: `src/Nwflash.Desktop/src-tauri/crates/nwflash-windows/src/process.rs`

**Interfaces:**
- Consumes: spawned `Child` stdout/stderr handles and cancellation callback.
- Produces: complete `ProcessOutput` without pipe-buffer deadlock.

- [ ] **Step 1: Write a failing high-output regression test**

```rust
#[test]
fn run_command_collects_large_stdout_before_the_timeout() {
    let output = run_command_with_timeout(
        ProcessCommand::new(
            "cmd",
            [
                "/C".to_string(),
                "for /L %i in (1,1,20000) do @echo 0123456789abcdef".to_string(),
            ],
        ),
        Some(Duration::from_secs(3)),
    )
    .unwrap();

    assert!(output.stdout.len() > 64 * 1024);
}
```

- [ ] **Step 2: Run the test red**

Run: `cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml -p nwflash-windows run_command_collects_large_stdout_before_the_timeout`

Expected: FAIL with the existing timeout because output pipes are not drained until exit.

- [ ] **Step 3: Implement concurrent pipe readers**

Take each piped handle immediately after `spawn`, drain it in a reader thread, preserve the existing poll/cancel/timeout loop, reap the child, then join readers into `ProcessOutput`.

- [ ] **Step 4: Run focused test green**

Run: `cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml -p nwflash-windows run_command_collects_large_stdout_before_the_timeout`

Expected: PASS.

### Task 5: Bind ROOT Selection to Inspected Bytes

**Files:**
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/Cargo.toml`
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/root.rs`
- Test: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/root.rs`

**Interfaces:**
- Consumes: a selected `.img` or `.bin` file.
- Produces: an opaque selection whose private SHA-256 fingerprint must match before preflight, patch, or automatic ROOT use.

- [ ] **Step 1: Write a failing mutation regression test**

```rust
#[test]
fn selected_root_image_rejects_changed_bytes_before_use() {
    let root = unique_test_directory("root-image-fingerprint");
    let image_path = root.join("init_boot.img");
    fs::create_dir_all(&root).unwrap();
    fs::write(&image_path, b"original image bytes").unwrap();

    let runtime = RootImageRuntime::new();
    let selection = inspect_root_image(&image_path).unwrap();
    let dto = runtime.replace_inspected(RootImageKind::InitBoot, selection).unwrap();
    fs::write(&image_path, b"changed image bytes").unwrap();

    assert!(runtime.get(RootImageKind::InitBoot, &dto.id).is_err());
    fs::remove_dir_all(root).unwrap();
}
```

- [ ] **Step 2: Run the test red**

Run: `cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml -p nwflash-tauri selected_root_image_rejects_changed_bytes_before_use`

Expected: FAIL because the runtime stores only the mutable path and size.

- [ ] **Step 3: Implement fingerprint verification**

Compute SHA-256 during native selection; store it only in `RootImageSelection`; recompute before all production retrieval and automatic workflow stages. Preserve DTO path secrecy and return the existing reselect-required error class when bytes differ.

- [ ] **Step 4: Run focused test green**

Run: `cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml -p nwflash-tauri selected_root_image_rejects_changed_bytes_before_use`

Expected: PASS.

### Task 6: Synchronize Architecture Documentation and Quality Gates

**Files:**
- Modify: `docs/architecture-tauri-migration.md`
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-domain/src/partition.rs`
- Modify: rustfmt-reported Rust files only if formatting remains required after the scoped changes

**Interfaces:**
- Consumes: final command boundary and resource behavior.
- Produces: documentation that matches the compiled implementation and a clean lint/format baseline.

- [ ] **Step 1: Update architecture invariants**

Document that bearer tokens, raw plans, arbitrary firmware URLs, ROM URLs, command arrays, and resource paths remain Rust-internal; document verified scrcpy metadata and corrupt-cache recovery.

- [ ] **Step 2: Remove strict-Clippy violations**

Replace each `Ok(create_plan(...)?)` with the direct `create_plan(...)` result at the three reported lines.

- [ ] **Step 3: Run full verification**

Run: `cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --workspace --quiet`

Run: `cargo fmt --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --all -- --check`

Run: `cargo clippy --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --workspace --all-targets -- -D warnings`

Expected: all commands exit 0.
