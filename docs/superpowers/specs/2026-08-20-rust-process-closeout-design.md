# Rust Process Runner Closeout Design

**Status:** Approved by the user's instruction to continue the recommended local closeout.

## Goal

Close the two verified local Rust gaps without changing device-facing behavior:

1. restore the exact SHA-256 declaration for the bundled `fastboot.exe` so the platform-tools integrity gate accepts the shipped binary; and
2. complete the process-pipe implementation so every spawned reader is reaped after a child is reaped, normal completion surfaces reader failures, and cancellation/timeout retains its existing user-facing result.

## Scope and boundaries

The change is limited to the Tauri platform-tools manifest, the Windows process runner and its tests, plus documentation that incorrectly says concurrent pipe draining is absent.

It does not alter Android platform-tool binaries, command construction, device selection, flash/root workflows, Cloudflare code, release signing, installers, or any real-device acceptance item.  Those operations remain outside this local task and require their existing approvals and credentials.

## Chosen approach

Three options were considered:

1. Correct only the manifest typo.  This unblocks the current test but leaves Task 4's cancellation and reader-error boundary incomplete.
2. **Recommended: close the manifest and process-runner boundary together.**  This is the smallest change that makes the documented Task 4 behavior true and gives the workspace a clean local regression gate.
3. Attempt release, signing, installation, and device acceptance.  That would require certificates, a graphical/installer environment, and explicit approved test hardware, so it is deliberately excluded.

The implementation uses option 2.

## Design

### Platform-tools manifest

Only the `fastboot.exe` digest text in `src/Nwflash.Desktop/src-tauri/resources/platform-tools/PLATFORM_TOOLS.SHA256` changes.  Its value must exactly equal both the existing Rust-pinned digest and the SHA-256 computed from the existing bundled executable.  The existing `shipped_platform_tools_manifest_matches_binaries` test is the regression proof; no binary is replaced or regenerated.

### Process pipe lifecycle

`process.rs` already starts a thread per piped stdout/stderr stream immediately after spawning a child, preventing OS pipe-buffer deadlocks during normal completion.  The closeout makes the result of each reader explicit instead of silently converting an I/O error or a panicked thread to an empty byte vector.

After a normal child exit, the runner will join every reader before creating `ProcessOutput`.  Any reader failure becomes a generic `DomainError::ExternalTool` output-read error; it will not report a partial stream as complete.  Joining all readers before returning ensures one failed reader cannot leave another detached.

After a cancellation or timeout, the runner will terminate and reap the child, then join all readers before returning the existing cancellation or timeout error.  Reader failures caused by the intentional process termination do not replace the requested cancellation/timeout outcome.  This preserves current operation-coordinator semantics while ensuring no reader remains detached.

The file-stdout variant follows the same rule for its stderr reader.  The normal stdout/stderr variant follows it for both readers.  No child output contents, paths, or command arguments are newly exposed to the frontend.

### Tests

Tests stay in the existing `process.rs` module and use deterministic local fixtures:

- retain the large stdout and large stderr regression tests for normal completion;
- add timeout and cancellation cases with active large pipe output, asserting prompt return and the existing typed error;
- add a direct reader-failure regression using an in-module failing `Read` fixture, asserting normal completion returns an external-tool error rather than partial success;
- retain the platform-tools manifest test and verify it changes from red to green once the digest text is corrected.

## Documentation and verification

Update only the statements that explicitly claim concurrent pipe draining is not implemented, including the Rust/Tauri architecture document and migration overview.  Historical plans are not broadly rechecked; only this completed Task 4 boundary is made accurate.

Verification must include:

```powershell
cargo fmt --check --all
cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml -p nwflash-windows
cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --workspace --no-fail-fast
cargo clippy --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --workspace -- -D warnings
npm --prefix src/Nwflash.Desktop run test
npm --prefix src/Nwflash.Desktop run build
```

The task is complete only when the corrected manifest is verified against the current binary, all pipe readers are joined on every exit path, the added regressions pass, and the above local checks are green.  Native WDIO, signed installers, and real-device tests are reported separately rather than claimed by these checks.
