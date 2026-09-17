# HTTP(S) Firmware Extraction Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Allow the firmware extraction workbench to inspect and extract firmware from any user-provided HTTP or HTTPS URL, including ZIP archives containing direct `.img`/`.bin` members and payload-based packages.

**Architecture:** Keep URL handling and remote ZIP Range reads in `nwflash-infrastructure`. Add a Tauri command adapter that stores checked remote entries in the existing opaque runtime and passes payload URLs unchanged to the existing `payload_dumper`, which Range-reads only the selected partition data. Extend the existing React page with a local/HTTP(S) source mode while keeping paths and URLs out of rendered metadata and operation logs.

**Tech Stack:** Rust workspace, Tauri commands, `reqwest`, `zip`, `tokio`, existing `OtaDownloader`/`FirmwareExtractService`, React, TypeScript, Vitest.

## Global Constraints

- Accept only URLs whose scheme is exactly `http` or `https`.
- Preserve query parameters in signed URLs but never return the complete URL in DTOs or page text.
- Direct-image ZIP extraction uses HTTP Range and only downloads selected members.
- Payload URLs are passed unchanged to `payload_dumper`; Rust does not stage the complete remote payload.
- Existing local extraction commands and opaque IDs remain backward compatible.
- Do not touch `cloudflare/**`, reset unrelated worktree changes, or commit implementation changes.

---

### Task 1: Harden Remote Firmware Primitives

**Files:**
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/src/remote_firmware.rs`
- Test: `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/tests/remote_firmware.rs`

**Interfaces:**
- `remote_firmware::probe_remote_kind`, `list_zip_members`, and `extract_zip_members` remain the public synchronous Range API.
- URL validation returns a stable invalid-input error for empty, malformed, or non-HTTP(S) URLs.

- [ ] **Step 1: Add failing URL validation tests.** Assert malformed URLs and empty strings are rejected while both HTTP and signed HTTPS URLs remain accepted.
- [ ] **Step 2: Run focused Rust tests and verify the new cases fail for the expected validation reason.**
- [ ] **Step 3: Implement scheme-aware URL parsing without putting the complete URL into user-facing errors.** Keep Range response validation strict for direct ZIP reads.
- [ ] **Step 4: Add a payload URL regression test for an HTTP resource served through the existing test server.** Assert inspection and extraction pass the original URL to the external command builder and do not create a complete-payload staging file.
- [ ] **Step 5: Run `cargo test -p nwflash-infrastructure --test remote_firmware` and keep the focused suite green.**

### Task 2: Add Remote Firmware Command Adapters

**Files:**
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/firmware.rs`
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/lib.rs`
- Test: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/firmware.rs`

**Interfaces:**
- Add `firmware_inspect_remote(state, url) -> Result<FirmwareInspectionDto, String>`.
- Add `firmware_extract_remote(state, url, selected_ids, output_directory_id) -> Result<FirmwareExtractionDto, String>`; the current implementation resolves the ID from a Rust-owned native-dialog selection runtime.
- Direct ZIP entries use checked opaque IDs; payload entries use `PayloadInspectionRuntime` and the same checked IDs as local payload extraction.

- [ ] **Step 1: Add failing command-level tests for direct-image URL inspection and extraction.** Use the existing Range test server and assert DTOs contain only safe entry names, sizes, and generated result IDs.
- [ ] **Step 2: Add failing tests proving `FirmwareExtractService::inspect_payload` and `extract_payload_with_expected_sizes_and_progress` receive the original payload URL.** Assert an HTTP URL is passed directly to the external command builder and no full package is downloaded first.
- [ ] **Step 3: Add failing tests for invalid selection and error redaction.** A URL, query string, or temporary path must not occur in the returned error string.
- [ ] **Step 4: Implement a remote inspection helper.** Probe the remote kind, list direct ZIP image members for `DirectImageZip`, or route the original payload URL through the existing payload inspection operation. Store source and entries in a runtime so extraction cannot use unchecked IDs.
- [ ] **Step 5: Implement a remote extraction helper.** Use `extract_zip_members` for direct ZIPs and pass payload URLs directly to the existing payload extraction operation. Wire cancellation to the operation token and report `firmware:progress` without creating a full-payload staging root.
- [ ] **Step 6: Register both commands in `tauri::generate_handler!` and run `cargo test -p nwflash-tauri --lib firmware`.**

### Task 3: Add HTTP(S) Source Mode to the Workbench

**Files:**
- Modify: `src/Nwflash.Desktop/src/pages/FirmwareExtractPage.tsx`
- Modify: `src/Nwflash.Desktop/src/pages/FirmwareExtractPage.test.tsx`

**Interfaces:**
- Local mode continues invoking `firmware_inspect_local`, `firmware_extract_vivo_local`, and `firmware_extract_payload_local`; extraction sends the Rust-issued `{ outputDirectoryId }` instead of a raw output path.
- Remote mode invokes `firmware_inspect_remote` and `firmware_extract_remote` with `{ url, selectedIds, outputDirectoryId }`; raw output paths are not remote execution inputs.
- Rendered source status is a generic label such as “已选择 HTTP(S) 固件地址”; the URL input is never echoed elsewhere.

- [ ] **Step 1: Add failing Vitest cases for switching to HTTP(S) mode, entering HTTP and signed HTTPS URLs, and checking them with `firmware_inspect_remote`.** Assert the command receives the exact URL and the page renders only safe format/entry data.
- [ ] **Step 2: Add failing cases for direct `.img`/`.bin` entries, payload entries, output-directory selection, and remote extraction command arguments.**
- [ ] **Step 3: Add failing cases for rejecting blank/non-HTTP(S) input in the UI and for hiding the URL from status/error/result text.**
- [ ] **Step 4: Implement the source-mode state and accessible segmented controls.** Keep local selection behavior unchanged, clear stale inspected entries when the source mode or URL changes, and disable conflicting actions while an operation runs.
- [ ] **Step 5: Route inspect/extract actions through the matching local or remote command and preserve existing progress/cancel/flash-preparation behavior.**
- [ ] **Step 6: Run `npm test -- --run src/pages/FirmwareExtractPage.test.tsx` and verify the existing local cases remain green.**

### Task 4: Verify Integration and Build Artifacts

**Files:**
- Modify: `src/Nwflash.Desktop/src/pages/FirmwareExtractPage.test.tsx` only if integration assertions reveal a real regression.
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/firmware.rs` only if compile/test output identifies an implementation defect.

**Interfaces:**
- No new public API beyond the two registered Tauri commands.

- [ ] **Step 1: Run `cargo fmt --check` and format only touched Rust files if required.**
- [ ] **Step 2: Run the focused infrastructure, application, and Tauri Rust tests.**
- [ ] **Step 3: Run the complete frontend test suite and TypeScript/Vite build from `src/Nwflash.Desktop`.**
- [ ] **Step 4: Run the Tauri release compilation command used by the repository and inspect the generated diff for unintended files.**
- [ ] **Step 5: Re-check the requirement list: HTTP(S) input, direct image ZIP extraction, direct payload URL extraction, cancellation behavior, URL/error redaction, and unchanged local flow.**
