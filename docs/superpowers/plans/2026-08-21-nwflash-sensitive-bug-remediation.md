# NWFlash Sensitive Bug Remediation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make Safe Flash, online OTA transfer, ROOT OTA extraction, and payload-dumper provisioning fail closed for the confirmed safety and integrity defects in the approved remediation design.

**Architecture:** The patch keeps device decisions in the application layer, enforces transfer bytes and catalog integrity in infrastructure, and maps catalog metadata only at the Tauri boundary. It does not introduce browser-side trust or change cancellation/authorization policy. Each task adds a regression that demonstrates the old unsafe path, then applies the smallest fail-closed implementation.

**Tech Stack:** Rust 2021, Tokio, Reqwest, Zip, SHA-256, Tauri 2, existing Cargo integration/unit tests.

## Global Constraints

- Preserve all unrelated dirty/untracked migration work; stage and commit only files owned by the task.
- Do not run destructive workspace cleanup or delete artifacts outside task-created temporary test directories.
- Treat unknown slot, `has-slot`, or `is-userspace` probe state as an execution error before any `fastboot flash` command.
- Online Safe Flash requires a positive catalog size and a 64-hex-character SHA-256; local sources remain unchanged.
- Keep external command arguments as arrays and preserve Chinese user-facing error boundaries.
- Every production behavior change must be preceded by a regression test observed failing against the old implementation.
- At each task boundary run its targeted Cargo test command, then request and address an independent review before starting the next dependent task.

---

## File Structure

| File | Responsibility in this plan |
| --- | --- |
| `crates/nwflash-application/src/safe_flash.rs` | Verify fastbootd and resolve slot targets fail closed before flashing. |
| `crates/nwflash-application/tests/safe_flash.rs` | Fake-executor regressions for fastbootd/slot probe outcomes. |
| `crates/nwflash-infrastructure/src/payload_provisioner.rs` | Contain payload-dumper ZIP members inside the owned staging root. |
| `crates/nwflash-infrastructure/src/ota_download.rs` | Enforce transport byte ceiling and catalog-integrity verification before publishing an OTA. |
| `crates/nwflash-infrastructure/src/lib.rs` | Re-export the new OTA integrity descriptor and verified-download API. |
| `crates/nwflash-infrastructure/tests/ota_download.rs` | HTTP regressions for oversized bodies and mismatching catalog hashes. |
| `crates/nwflash-application/src/safe_flash.rs` | Carry online catalog integrity into the OTA downloader. |
| `crates/nwflash-tauri/src/commands/safe_flash.rs` | Validate API catalog metadata and construct the owned online source. |
| `crates/nwflash-tauri/src/commands/safe_flash.rs` test module | Verify invalid catalog integrity is rejected before preparation. |
| `crates/nwflash-application/src/root_ota.rs` | Reserve sufficient staging capacity for selected direct-ZIP ROOT images. |
| `crates/nwflash-application/tests/root_ota.rs` | Inject a fixed disk provider and verify no image directory is created on capacity rejection. |

### Task 1: Fail Closed for Safe Flash Slot and fastbootd Probes

**Files:**
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/src/safe_flash.rs:145-260,360-410`
- Test: `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/tests/safe_flash.rs:131-330,492-565`

**Interfaces:**
- Consumes: `DeviceTransport::build_fastboot_getvar_command`, `SafeFlashSlotMode`, `parse_fastboot_var_output`, `is_affirmative_flag`.
- Produces: `SafeFlashExecutionService::execute` that returns `DomainError` before any `flash` when userspace/slot facts are not confirmed.

- [ ] **Step 1: Add failing regression tests for unknown device mode and unknown slot facts**

Add three `RecordedExecutor` tests, each asserting no recorded command has `args[2] == "flash"`:

```rust
#[test]
fn execution_rejects_bootloader_fastboot_before_any_flash() {
    let executor = RecordedExecutor::new([
        successful_output("FASTBOOT-001\tfastboot\n"),
        successful_output("(bootloader) is-userspace: no\n"),
    ]);
    let error = SafeFlashExecutionService::new(Arc::new(executor.clone()))
        .execute(request_for_other_slot(&source, &options, false), || false, |_| {}, |_| {})
        .expect_err("bootloader fastboot must not be accepted as fastbootd");
    assert!(error.to_string().contains("fastbootd"));
    assert!(!executor.commands().iter().any(|command| command.args.get(2) == Some(&"flash".to_string())));
}
```

Repeat with `is-userspace: yes`, then a nonzero `current-slot` result; repeat with a successful current slot and a nonzero `has-slot:boot` result. Both slot cases must error before partition probing or flashing. Update the existing happy-path fixture so the fake executor returns `fastboot devices`, `is-userspace: yes`, `current-slot: a`, and `has-slot:boot: yes` in the real command order.

- [ ] **Step 2: Run the new tests to verify RED**

Run:

```powershell
cargo test -p nwflash-application --test safe_flash
```

Expected: the new tests fail because an already Fastboot-connected device skips `is-userspace`, and slot read errors fall back to bare partition targets.

- [ ] **Step 3: Implement explicit userspace and slot proof**

Make `wait_for_fastbootd` accept `&DeviceTransport` and return only after both facts are true:

```rust
if has_fastboot_device_serial(&output.stdout, expected_serial) {
    let userspace = self.read_fastboot_var(transport, expected_serial, "is-userspace", is_canceled)?;
    if is_affirmative_flag(&userspace) {
        return Ok(expected_serial.to_string());
    }
}
```

Call it unconditionally after an optional ADB reboot, so pre-existing Fastboot connections are verified too. For `OtherSlot`, call `read_fastboot_var("current-slot")?`, then require `normalize_slot_name` to return `Some`; do not use `.ok()`. For every slot-based mode, propagate `read_fastboot_var("has-slot:<partition>")?` instead of `unwrap_or(false)`. Keep a successful false `has-slot` response as the only slotless path.

- [ ] **Step 4: Run targeted Safe Flash tests to verify GREEN**

Run:

```powershell
cargo test -p nwflash-application --test safe_flash
```

Expected: all Safe Flash tests pass, including the new fail-closed cases and existing real stderr parsing coverage.

- [ ] **Step 5: Commit the isolated task**

```powershell
git add -- 'src/Nwflash.Desktop/src-tauri/crates/nwflash-application/src/safe_flash.rs' 'src/Nwflash.Desktop/src-tauri/crates/nwflash-application/tests/safe_flash.rs'
git commit -m "fix(safe-flash): require verified fastbootd slot probes"
```

### Task 2: Contain Payload-Dumper ZIP Member Paths

**Files:**
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/src/payload_provisioner.rs:227-270,273-320`
- Test: `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/src/payload_provisioner.rs` inline test module

**Interfaces:**
- Consumes: ZIP entry name strings and a caller-owned `destination_root`.
- Produces: `extract_archive_safely` that writes only validated relative normal paths or returns `PayloadProvisionError::Integrity`.

- [ ] **Step 1: Add failing ZIP escape regressions**

Add a test helper that writes a small ZIP fixture and invoke the private extractor directly. Cover both a backslash-rooted member and a UNC-like member:

```rust
#[test]
fn payload_archive_rejects_backslash_rooted_and_unc_members() {
    for name in [r"\nwflash-escape.txt", r"\\127.0.0.1\share\probe"] {
        let root = temporary_payload_fixture_root();
        let archive = write_zip_fixture(&root, &[(name, b"bad")]);
        let staging = root.join("staging");
        let error = extract_archive_safely(&archive, &staging)
            .expect_err("rooted archive member must be rejected");
        assert!(matches!(error, PayloadProvisionError::Integrity(_)));
        assert!(!staging.exists());
        std::fs::remove_dir_all(root).expect("fixture root should be removed");
    }
}
```

Also retain a relative nested member fixture such as `bin/payload_dumper.exe` to prove legitimate archives still extract.

- [ ] **Step 2: Run the focused tests to verify RED**

Run:

```powershell
cargo test -p nwflash-infrastructure payload_archive_rejects_backslash_rooted_and_unc_members
```

Expected: the current extractor accepts at least the backslash-rooted name after converting it to `/...`.

- [ ] **Step 3: Normalize once and accept only normal relative components**

First scan and validate every ZIP entry name before creating `destination_root`, then extract in a second pass. Add a small helper with this contract:

```rust
fn safe_archive_relative_path(name: &str) -> Result<PathBuf, PayloadProvisionError> {
    let normalized = name.replace('\\', "/");
    let path = Path::new(&normalized);
    if normalized.is_empty() || path.is_absolute()
        || !path.components().all(|part| matches!(part, Component::Normal(_))) {
        return Err(PayloadProvisionError::Integrity("非法 zip 条目路径。".to_string()));
    }
    Ok(path.to_path_buf())
}
```

Use the returned relative `PathBuf` for every `destination_root.join(...)`; check directory entries by the validated path rather than raw slash suffixes. Do not create the output root or files until the full validation pass succeeds.

- [ ] **Step 4: Run payload-provisioner tests to verify GREEN**

Run:

```powershell
cargo test -p nwflash-infrastructure payload_provisioner::tests
```

Expected: the new escape regressions and existing cache-integrity tests pass.

- [ ] **Step 5: Commit the isolated task**

```powershell
git add -- 'src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/src/payload_provisioner.rs'
git commit -m "fix(resources): reject rooted payload archive members"
```

### Task 3: Enforce OTA Byte Ceilings and Catalog Integrity

**Files:**
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/src/ota_download.rs:1-115,655-730`
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/src/lib.rs:36-44`
- Test: `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/tests/ota_download.rs:126-450`

**Interfaces:**
- Consumes: an authoritative expected OTA length and SHA-256 from a trusted catalog.
- Produces: `OtaExpectedIntegrity`, `OtaDownloader::download_to_file_with_integrity`, and a public cancellation wrapper that verifies the final staged file before promotion.

- [ ] **Step 1: Add failing oversized-body and wrong-hash download tests**

Use Wiremock to return `HEAD Content-Length: 5` and a single-connection `GET` body longer than five bytes. Pre-create `destination` as `b"old"`:

```rust
let error = downloader.download_to_file(&server.uri(), &destination, 1, &CancellationToken::new(), None)
    .await
    .expect_err("body larger than the probed length must fail before excess bytes are written");
assert!(error.to_string().contains("超过"));
assert_eq!(fs::read(&destination).expect("approved destination"), b"old");
assert!(!staging_download_path(&destination, nonce).expect("staging path").exists());
```

Add a second test using `OtaExpectedIntegrity::from_catalog(Some(5), Some(VALID_SHA))` with an equal-length but wrong-byte response. It must fail hash verification and preserve the destination. Add constructor tests rejecting `None`, zero/negative size, non-hex, and non-64-character hash strings.

- [ ] **Step 2: Run the new infrastructure tests to verify RED**

Run:

```powershell
cargo test -p nwflash-infrastructure --test ota_download
```

Expected: the oversized body is written until EOF and catalog-integrity APIs do not exist yet.

- [ ] **Step 3: Add the integrity descriptor and bounded write path**

Define an owned descriptor and strict constructor:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OtaExpectedIntegrity { pub length: u64, pub sha256: String }

impl OtaExpectedIntegrity {
    pub fn from_catalog(size_bytes: Option<i64>, sha256: Option<&str>) -> Result<Self, OtaDownloadError> { /* positive u64 + 64 ASCII hex */ }
}
```

Before every `write_all`, calculate `downloaded.checked_add(chunk.len() as u64)` and return `OtaDownloadError::Download` when it exceeds `total_bytes`. Add `download_to_file_with_integrity` that requires the probe length to equal `integrity.length`, calls the existing staged download path, computes SHA-256 of staging, and promotes only if both checks pass. Keep the existing no-integrity APIs for local/noncatalog callers. Re-export the descriptor and verified helper from `lib.rs`.

- [ ] **Step 4: Run the complete OTA downloader test file to verify GREEN**

Run:

```powershell
cargo test -p nwflash-infrastructure --test ota_download
```

Expected: all existing range, cancellation, capacity, and replacement tests remain green, while oversized and wrong-hash inputs leave no staging artifact.

- [ ] **Step 5: Commit the isolated task**

```powershell
git add -- 'src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/src/ota_download.rs' 'src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/src/lib.rs' 'src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/tests/ota_download.rs'
git commit -m "fix(ota): enforce transfer size and catalog integrity"
```

### Task 4: Propagate Catalog Integrity into Online Safe Flash

**Files:**
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/src/safe_flash.rs:70-82,651-690,1054-1075`
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/safe_flash.rs:350-425`
- Test: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/safe_flash.rs` inline test module

**Interfaces:**
- Consumes: Task 3 `OtaExpectedIntegrity::from_catalog` and verified OTA downloader API.
- Produces: `SafeFlashSource::Online { integrity: OtaExpectedIntegrity, .. }`; no online preparation begins without validated catalog metadata.

- [ ] **Step 1: Add failing command-boundary metadata tests**

Extract the small mapping into a private helper such as `online_ota_integrity(&RomResolveResponse) -> Result<OtaExpectedIntegrity, DomainError>`. Test it directly with catalog values:

```rust
#[test]
fn online_safe_flash_rejects_catalog_without_pinned_integrity() {
    let rom = RomResolveResponse { pd: "PD".into(), version: "V".into(), url: "https://example.test/ota.zip".into(), name: None, size_bytes: None, sha256: None };
    let error = online_ota_integrity(&rom).expect_err("online OTA must be catalog-pinned");
    assert!(error.to_string().contains("完整性"));
}
```

Add the valid 64-hex and invalid hash/negative-size cases. In application tests, use a small remote source plus a mismatched descriptor and assert no prepared flash source is returned.

- [ ] **Step 2: Run the new tests to verify RED**

Run:

```powershell
cargo test -p nwflash-tauri online_safe_flash_rejects_catalog_without_pinned_integrity
cargo test -p nwflash-application --test safe_flash online_source_rejects_catalog_hash_mismatch
```

Expected: the helper and descriptor field are absent, and online source preparation currently ignores catalog metadata.

- [ ] **Step 3: Carry only validated metadata into the owned source**

Construct `OtaExpectedIntegrity` immediately after `resolve_rom`, before the payload provisioner/download begins:

```rust
let integrity = OtaExpectedIntegrity::from_catalog(rom.size_bytes, rom.sha256.as_deref())
    .map_err(|error| DomainError::InvalidOperation(format!("在线 OTA 完整性信息无效：{error}")))?;
```

Add `integrity` to the Online variant, thread it through `resolve_source_with_cancellation_and_progress` to `resolve_online_source`, and call Task 3's verified OTA download function. Do not serialize or expose the hash through preflight DTOs.

- [ ] **Step 4: Run focused application and Tauri tests to verify GREEN**

Run:

```powershell
cargo test -p nwflash-application --test safe_flash
cargo test -p nwflash-tauri commands::safe_flash::tests
```

Expected: online preparation rejects missing/mismatched catalog integrity, valid local and online fixtures still pass, and DTO secrecy tests remain green.

- [ ] **Step 5: Commit the dependent task**

```powershell
git add -- 'src/Nwflash.Desktop/src-tauri/crates/nwflash-application/src/safe_flash.rs' 'src/Nwflash.Desktop/src-tauri/crates/nwflash-application/tests/safe_flash.rs' 'src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/safe_flash.rs'
git commit -m "fix(safe-flash): require catalog-pinned online ota"
```

### Task 5: Reserve Capacity for Direct-ZIP ROOT OTA Extraction

**Files:**
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/src/root_ota.rs:1-14,216-255`
- Test: `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/tests/root_ota.rs:1-220`

**Interfaces:**
- Consumes: `ZipMember.size_bytes`, `OtaDiskSpaceProvider`, and `validate_available_space`.
- Produces: `RootOtaService::with_disk_space(Arc<dyn OtaDiskSpaceProvider>)` for deterministic tests; `RootOtaService::new()` retains the system provider.

- [ ] **Step 1: Add a failing insufficient-space direct-ZIP regression**

Add a fixed provider and construct the service with fewer bytes than a `boot.img` plus `vendor_boot.img` fixture:

```rust
let service = RootOtaService::with_disk_space(Arc::new(FixedDiskSpace(1)));
let error = service.extract(options_for(&url, &root), || false, |_| {}, |_| {})
    .expect_err("direct zip must reserve extraction capacity before writing");
assert!(error.to_string().contains("磁盘空间不足"));
assert!(!root.join("images").exists());
```

Add a second test whose fixed provider has exactly the checked aggregate size and retains the existing boot/vendor extraction assertions.

- [ ] **Step 2: Run the new ROOT OTA tests to verify RED**

Run:

```powershell
cargo test -p nwflash-application --test root_ota
```

Expected: the insufficient-capacity case currently writes image output because `RootOtaService` has no disk provider or preflight.

- [ ] **Step 3: Inject disk provider and check selected archive metadata**

Change the service to own `Arc<dyn OtaDiskSpaceProvider>`, with `new()` using `SystemOtaDiskSpaceProvider` and `with_disk_space` for tests. In `extract_from_direct_zip`, calculate the selected `init_boot`, `boot`, and `vendor_boot` total with `u64::try_from` plus `checked_add`; reject invalid metadata and call `validate_available_space(required, available)` before `extract_zip_members`.

- [ ] **Step 4: Run the complete ROOT OTA integration suite to verify GREEN**

Run:

```powershell
cargo test -p nwflash-application --test root_ota
```

Expected: all direct ZIP, payload, cancellation, progress, and new capacity tests pass.

- [ ] **Step 5: Commit the isolated task**

```powershell
git add -- 'src/Nwflash.Desktop/src-tauri/crates/nwflash-application/src/root_ota.rs' 'src/Nwflash.Desktop/src-tauri/crates/nwflash-application/tests/root_ota.rs'
git commit -m "fix(root): reserve space before remote zip extraction"
```

## Final Verification

- [ ] Run `cargo fmt --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --check`.
- [ ] Run `cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --workspace --no-fail-fast`.
- [ ] Run `cargo clippy --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --workspace --all-targets -- -D warnings`.
- [ ] Inspect `git diff --check` and `git status --short`; ensure only task-owned files are staged/committed.
- [ ] Dispatch an independent reviewer for implementation correctness, sensitive-boundary behavior, and test coverage; resolve all Critical/Important findings before completion.
