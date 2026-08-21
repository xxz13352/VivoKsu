# NWFlash Sensitive Bug Remediation Design

**Date:** 2026-08-21

## Goal

Remove the confirmed release-blocking correctness and security failures in Rust NWFlash without changing the product policies that still need an explicit decision.

## Evidence and scope

The workspace-wide Rust baseline passed before this design was written:

```text
cargo test --workspace --no-fail-fast
```

The first remediation stage covers only defects with a deterministic unsafe outcome:

1. Safe Flash silently falls back to an unsuffixed partition when a slot probe fails.
2. Safe Flash can begin a full flash while the target is ordinary bootloader Fastboot rather than fastbootd.
3. The payload-dumper ZIP extractor accepts Windows backslash-rooted and UNC member names after its first path check.
4. A single-connection OTA response can write past the previously probed content length before rejecting it.
5. Online Safe Flash discards catalog-provided SHA-256 and size metadata, so the downloaded OTA is not bound to the catalog response.
6. Direct-ZIP ROOT OTA extraction does not reserve disk space for the selected images before writing them.

This stage deliberately excludes policy or larger architecture changes:

- whether a running `fastboot flash` may ever be forcibly cancelled;
- replacing generic Quick Flash browser-held plans with Rust-owned one-shot confirmation capabilities;
- binding every prepared artifact to a content fingerprint and authenticated session;
- remote URL redirect/private-network policy and range-response representation pinning;
- process-tree supervision and bounded stdout/stderr capture.

Those items remain tracked as sensitive follow-up work rather than being silently folded into a bug-fix patch.

## Design

### 1. Fail closed for Safe Flash slot and fastbootd probes

`SafeFlashExecutionService` will distinguish a valid negative `has-slot:<partition>` answer from a failed probe.

- `OtherSlot` requires a successfully parsed `current-slot` value before any partition command is built.
- Every slot-sensitive mode requires `has-slot:<partition>` to succeed before deciding between the bare partition and `_a`/`_b` targets.
- A malformed slot value is an execution error, not `None`.
- Before flashing, the service will verify `fastboot getvar is-userspace` reports a true value. This check runs both after an ADB-to-fastbootd transition and when the device was already reported as Fastboot-connected.
- A failed or non-userspace probe stops before the first `flash` command.

The existing behavior for genuinely slotless partitions remains: after a successful `has-slot` answer of false, the bare partition is flashed once.

### 2. Contain every payload-dumper ZIP member below its staging root

`extract_archive_safely` will normalize ZIP names before resolving them and allow only relative normal path components.

- Convert ZIP backslashes to forward slashes before validation.
- Reject empty, rooted, prefixed, `.` and `..` components after normalization.
- Join only the validated relative path to the staging root.

This prevents `\\file`, `\\\\server\\share`, drive-prefix, and traversal names from becoming paths outside the owned staging directory. The pinned executable hash check remains in place after extraction.

### 3. Bound OTA writes and preserve an approved destination

`OtaDownloader` will treat the probe/catalog length as a hard byte ceiling.

- `write_response_to_file` calculates the next byte count before writing; a response chunk exceeding the known total returns an error without writing that chunk.
- Error paths remove the staging file and leave any existing destination untouched.

This is intentionally limited to the OTA download path, where an authoritative total length already exists. Generic resource maximum-size policy is deferred because several resources lack an immutable expected length.

An existing Windows regression already verifies successful replacement of a destination through the current Tokio rename path. The audit claim that every existing-destination replacement fails was not reproducible here, so no speculative publish-path rewrite is included.

### 4. Bind Online Safe Flash to catalog integrity metadata

The catalog response's optional `size_bytes` and `sha256` fields will become mandatory for online Safe Flash preflight.

- `SafeFlashSource::Online` carries a validated expected size and SHA-256 alongside its URL.
- The online command rejects a catalog response that lacks a positive, representable size or a well-formed SHA-256 digest.
- `OtaDownloader` accepts an expected integrity descriptor and, before promotion, verifies both final file length and SHA-256 against the catalog metadata.
- The existing remote server `Content-Length` probe remains a transport planning check; it does not replace catalog verification.

Local Safe Flash paths retain their existing local-source behavior because they do not have catalog metadata.

### 5. Reserve disk space for direct-ZIP ROOT OTA images

After listing only `init_boot`, `boot`, and `vendor_boot` members, `RootOtaService` will calculate a checked aggregate output size and query the existing `SystemOtaDiskSpaceProvider` for its owned staging root.

- Negative/overflowing member metadata produces an invalid-format error.
- Insufficient capacity fails before `extract_zip_members` creates its image output directory or writes an image.
- The existing cancellation and cleanup behavior remains unchanged.

## Interfaces and file boundaries

| Area | Primary files | Change boundary |
| --- | --- | --- |
| Safe Flash execution | `crates/nwflash-application/src/safe_flash.rs`, `tests/safe_flash.rs` | Live-device slot/fastbootd probe semantics only |
| Payload ZIP extraction | `crates/nwflash-infrastructure/src/payload_provisioner.rs` | ZIP member path validation only |
| OTA transfer | `crates/nwflash-infrastructure/src/ota_download.rs`, `tests/ota_download.rs` | Byte limit, integrity descriptor, transactional publish |
| Online Safe Flash mapping | `crates/nwflash-tauri/src/commands/safe_flash.rs`, command tests | Catalog DTO to owned source integrity propagation |
| ROOT OTA capacity | `crates/nwflash-application/src/root_ota.rs`, `tests/root_ota.rs` | Pre-extraction capacity validation |

## Test strategy

Each code change starts with a regression that fails on the current behavior.

1. Safe Flash fake executor: failed/malformed `current-slot`, failed `has-slot`, and `is-userspace=no` must each produce no `flash` command; a verified slotless partition remains supported.
2. Payload ZIP fixture: backslash-rooted and UNC-like names must be rejected before any output is created; a normal nested relative member remains extractable.
3. OTA wiremock/local server: a response whose body exceeds its probed length must fail before writing the excess and preserve a pre-existing destination; the existing successful-replacement regression remains green.
4. Online catalog fixture: missing/invalid integrity metadata is rejected; a same-length wrong-hash OTA fails before it is prepared for flashing; a matching artifact succeeds.
5. ROOT OTA pure capacity helper: insufficient capacity is rejected before image extraction; checked valid aggregate sizes continue to extract.

The targeted suites run first, followed by workspace formatting, full Rust tests, clippy with warnings denied, and an independent scope review.

## Risks and compatibility

- Online Safe Flash will fail closed for catalog records that lack valid size or SHA-256 metadata. This is intentional because such a record cannot establish artifact integrity.
- Safe Flash will stop instead of guessing when fastboot slot/userspace probes fail. The retry path is to restore a stable USB connection and re-run preflight.
- The patch does not alter any browser DTO fields unless the Rust mapping needs integrity values already present in the server response.
