# ROOT Patched Artifact Integrity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bind every production ROOT patched artifact to verified bytes and reject same-path, same-size replacement before any manual or automatic flash plan can use it.

**Architecture:** `RootPatchedArtifactRuntime` stores a SHA-256 for production-owned patch outputs and verifies it at its shared lookup boundary. Existing manual and automatic flows already resolve artifacts through this runtime, so they inherit the check without browser DTO changes. Test-only virtual fixture helpers remain isolated behind `#[cfg(test)]`.

**Tech Stack:** Rust 2021, SHA-256 (`sha2`), Tokio `spawn_blocking`, existing Tauri command unit tests.

## Global Constraints

- Preserve unrelated dirty/untracked migration work; do not stage, commit, reset, checkout, or rewrite unrelated files.
- Production ROOT patched artifact registration requires a SHA-256; test-only virtual paths may remain unverified only under `#[cfg(test)]`.
- Compute and verify hashes outside runtime mutexes; never expose hashes in DTOs.
- Retain existing device-serial binding, opaque IDs, single-use prepared flash plans, and owned-staging cleanup.
- A changed or unreadable artifact must return a localized error before any flash plan is built or consumed.
- Use TDD: every production change begins with a regression observed failing first.

---

## File Structure

| File | Responsibility |
| --- | --- |
| `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/root.rs` | Artifact fingerprint storage, verification, owned publication, and inline regressions. |

### Task 1: Fingerprint and Verify ROOT Patched Artifacts

**Files:**
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/root.rs:509-656,1360-1545,1798-1835,2526-2910`
- Test: inline `#[cfg(test)]` module in the same file

**Interfaces:**
- Consumes: existing `compute_root_image_fingerprint` pattern, `validate_patched_root_image`, `RootPatchedArtifactRuntime`, `automatic_root_flash_source`.
- Produces: production-owned patched artifacts carrying `fingerprint: Some(String)` and lookup methods that verify bytes before returning an artifact or prepared plan.

- [ ] **Step 1: Write failing same-size replacement regressions**

Add a small temporary-file helper in the root command test module and register it through a new verified-owned test helper. Cover both direct lookup and prepared-plan consumption:

```rust
#[test]
fn verified_root_patch_artifact_rejects_same_size_changed_bytes_before_use() {
    let root = temporary_root_patch_fixture();
    let image = root.join("patched.img");
    fs::write(&image, b"AAAA").expect("fixture image");
    let runtime = RootPatchedArtifactRuntime::new();
    let dto = runtime.replace_verified_for_test(
        RootImageKind::InitBoot,
        FlashImageInfo { path: image.to_string_lossy().into_owned(), size_bytes: 4 },
        QuickFlashPartition::InitBoot,
        "DEVICE-A".to_string(),
    ).expect("verified artifact");
    fs::write(&image, b"BBBB").expect("same-size replacement");
    assert!(runtime.get_for_device(&dto.artifact_id, "DEVICE-A").is_err());
}
```

Add a second regression that creates a valid `PartitionExecutionPlan`, calls `prepare_flash`, overwrites `AAAA` with `BBBB`, then uses a verified take helper and asserts it returns an error without consuming/executing the plan. Add automatic-source coverage with a changed artifact and an unchanged-byte happy path.

- [ ] **Step 2: Run RED**

Run:

```powershell
cargo test -p nwflash-tauri root_patched_artifact
```

Expected: current runtime resolves the changed same-size path because it stores no fingerprint; the verified registration/take helper does not exist yet.

- [ ] **Step 3: Add artifact fingerprint storage and shared verification**

Add `fingerprint: Option<String>` to `RootPatchedArtifact` and a patched-artifact verifier:

```rust
fn verify_root_patched_artifact(artifact: &RootPatchedArtifact) -> Result<(), String> {
    let Some(expected) = artifact.fingerprint.as_deref() else { return Ok(()); } // test-only virtual fixtures
    let actual = compute_root_patched_artifact_fingerprint(Path::new(&artifact.image.path))?;
    (actual == expected).then_some(()).ok_or_else(|| "ROOT 修补工件内容已变化，请重新修补。".to_string())
}
```

Make `get` clone the selected artifact while locked, release the lock, then call this verifier. Keep `get_for_device` layered after `get`. Add `take_verified_prepared_flash_for_device` that validates the current artifact/device before consuming the stored plan, and use it from `root_execute_patched_artifact_flash`.

Mark virtual `replace`/`replace_for_device` helpers `#[cfg(test)]`. Change production `replace_owned` to require the computed `String` fingerprint. In both patch-core `spawn_blocking` validation closures, return `(FlashImageInfo, fingerprint)` after validating the output and computing the digest, then pass the digest to `replace_owned`. On replacement, take state under the mutex, clear prepared flash, release the mutex, then remove only the prior owned staging root.

- [ ] **Step 4: Run GREEN**

Run:

```powershell
cargo test -p nwflash-tauri commands::root::tests
cargo fmt --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --check
```

Expected: changed-byte regressions reject before plan/source use; unchanged artifacts, device binding, automatic ROOT, staging cleanup, and DTO secrecy tests remain green.

- [ ] **Step 5: Commit only if ownership is explicit**

The target source is pre-existing untracked user work. Do not stage or commit it without explicit user authorization that includes its pre-existing content. Record test evidence in the SDD report instead.

## Final Verification

- [ ] Run `cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --workspace --no-fail-fast`.
- [ ] Run `cargo clippy --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --workspace --all-targets -- -D warnings`.
- [ ] Run an independent ROOT-scoped review for fingerprint checks, mutex/cleanup ordering, and manual/automatic execution paths.
