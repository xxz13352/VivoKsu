# Plan C Wave 2 Opaque Spool and Uploader Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use subagent-driven-development or executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Preimplement crash-safe local trace scheduling and ACK handling around protection-owned opaque `SealedTraceUpload` values, without giving infrastructure access to raw trace DTOs, serialized bodies, or redaction fragments.

**Architecture:** Wave 2 persists metadata only: canonical owner identity, item revisions, sealed-attempt identity, limits, state, and loss tombstones. Protection remains the sole owner of complete logical-stream redaction, output splitting/hashing, sealed request construction, and durable opaque sealed-upload storage. A future emitter receives only a concrete `SealedTraceUpload` plus its concrete authenticated session; Wave 2 returns metadata-only dispatch or remediation instructions and later applies the HTTP result against the exact captured attempt using revision compare-and-swap.

**Tech Stack:** Rust 2021, serde/serde_json for metadata manifests, sha2 for canonical username hashing, existing Windows `MoveFileExW` atomic replacement support, tokio tests. No new dependencies and no `Cargo.lock` change.

---

## Option B trust boundary

The current dirty Wave 1 worktree is only an expected interface. Its previous outbound serializer/proof trait is superseded. Wave 2 must not define or accept any of the following:

- raw or `Validated` trace DTOs;
- `RedactedTraceText` or a separately supplied stdout/stderr fragment;
- arbitrary JSON, UTF-8, request-body, or chunk/hash bytes;
- a serializer/redaction adapter trait, trait object, or fake proof type;
- a reusable sealed attempt after `begin_dispatch`.

The only future production bridge is a concrete protection adapter that receives a concrete `SealedTraceUpload`, obtains its protection-issued metadata and durable opaque lookup ID, and registers that metadata. Tests use `#[cfg(test)]` metadata factories; they never manufacture a payload and do not claim to prove redaction.

```rust
pub(crate) struct TraceOwnerGeneration {
    username_hash: [u8; 32],
    login_generation: u64,
}

pub(crate) struct ProtectionSealedUploadId([u8; 32]);

pub(crate) struct SealedItemRevision {
    key: TraceItemKey,
    trace_id: String,
    parent: Option<TraceItemKey>,
    revision: u64,
    created_at_ms: u64,
}

pub(crate) struct SealedAttemptManifest {
    attempt_id: String,
    owner: TraceOwnerGeneration,
    client_version_hash: [u8; 32],
    sealed_upload_id: ProtectionSealedUploadId,
    wire_bytes: u32,
    run_count: u16,
    event_count: u16,
    chunk_count: u16,
    items: Vec<SealedItemRevision>,
}
```

`ProtectionSealedUploadId` is not the V2 wire `upload_id`; it is an opaque durable protection-store lookup identity. Every protection seal for an HTTP attempt must produce a fresh opaque ID and a fresh wire `upload_id`. Stable entity IDs remain unchanged across attempts.

### Task 1: Revisioned metadata spool

**Files:**
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/src/trace_spool.rs`

- [ ] **Step 1: Write RED tests for identity, revisions, and atomic persistence**

Cover exact `username_hash + login_generation` isolation; scope/path traversal resistance; metadata Debug without owner names, token values, or payload; atomic reopen; failed manifest replacement preserving the prior state; monotonic revision registration; revision rollback/jump rejection; parent metadata preservation; attempt owner mismatch; and durable attempt state.

- [ ] **Step 2: Write RED tests for single-use attempts and CAS**

Required cases:

- `begin_dispatch` captures original owner, opaque sealed ID, and every `(entity,id,revision)` snapshot.
- An attempt is claimed once; after claim it is never dispatched again.
- Reopen with an in-flight attempt retires it and marks same-revision items `NeedsSeal`; it never reuses the old opaque ID.
- Recovery runs once per process/root startup epoch; normal scheduler reads never reclaim a live in-flight attempt, and multiple store instances for one canonical root share one mutation lock.
- Accepted run revision 1 deletes only that exact current revision and never its event/chunk children.
- If terminal run revision 2 is registered before the old open revision 1 ACK, the old ACK deletes nothing and cannot reschedule or mark remediation on revision 2.
- Registering a newer revision retires any still-pending sealed body containing the superseded revision and moves its other current items to `NeedsSeal`; `created_at_ms` is immutable across every revision.
- Response-time scope cannot be supplied to ACK application; the persisted attempt handle is the identity authority.

- [ ] **Step 3: Implement the metadata-only spool**

Implement one atomically replaced manifest per owner generation. Public safe result types may expose counts/status only; factories for `ProtectionSealedUploadId`, `SealedAttemptManifest`, and revisions remain crate-private, with test-only constructors behind `#[cfg(test)]`.

Core operations:

```rust
register_sealed_attempt(manifest)
due_pending_attempts(owner, now_ms)
due_reseal_items(owner, now_ms, current_client_version_hash)
due_remediations(owner, current_client_version_hash)
begin_dispatch(attempt_id, expected_owner) -> InflightAttemptHandle
apply_accepted_cas(handle, accepted_keys)
retire_attempt_and_mark_reseal_cas(handle, next_at_ms, reason)
mark_needs_remediation_cas(handle, affected_keys)
register_remediated_attempt(handle, affected_keys, replacement_manifest)
pause_owner_generation(owner, reason)
pause_client_version_for_update(handle)
expire(owner, now_ms)
```

`register_sealed_attempt` validates final protection-supplied limits (`wire_bytes <= 1_048_576`, runs <= 20, events <= 100, chunks <= 200), unique attempt/opaque IDs, unique items, and exact count agreement. Existing item revisions may advance only by one while retaining entity, trace, and parent identity.

The root also contains a durable client-version update gate. A 426 retires every pending sealed attempt with the captured blocked version hash across owners/generations and moves its current items to `NeedsSeal`. The blocked version cannot dispatch or reseal; a different current version hash may reseal the same owner's evidence. `begin_dispatch` rechecks this gate under the root lock.

All result mutations use the attempt snapshot revision as CAS. Accepted parent deletion is exact and never cascades. Rejected/unacknowledged/backoff/remediation changes also skip newer revisions. A used attempt is retired; retryable items become `NeedsSeal`, so future protection must register a brand-new sealed attempt and opaque ID before another dispatch.

- [ ] **Step 4: Implement retention loss and tombstones**

When any current item reaches seven days, first atomically persist a payload-free `retention_expired_7d` loss/tombstone keyed by owner+trace, then atomically remove that trace's items and attempts. If loss persistence fails, removal does not happen. If manifest replacement fails after the tombstone succeeds, reopen filters/repairs the tombstoned trace and never dispatches it. Later registration of that owner+trace fails closed.

- [ ] **Step 5: Run focused GREEN**

```powershell
cargo test -p nwflash-infrastructure trace_spool::tests --lib
```

Expected: all spool tests pass. Main agent stages only `trace_spool.rs` for `feat(trace): add revisioned opaque spool`.

### Task 2: Attempt-bound uploader state machine

**Files:**
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/src/trace_uploader.rs`

- [ ] **Step 1: Write RED tests for dispatch and response validation**

Use the real temporary metadata spool. Cover offline zero-claim; exact owner-generation claim; captured-owner instruction; single-use opaque sealed attempt; malicious ACKs; mixed ACK; parent non-cascade; old revision ACK; and fresh-seal requirement after every used attempt.

- [ ] **Step 2: Implement concrete metadata-only instructions**

No transport or wire adapter trait is allowed. Implement:

```rust
TraceUploader
UploadConnectivity
DispatchInstruction { handle, owner, sealed_upload_id }
TraceHttpOutcome
TraceUploadAck
TraceRejectedItem
UploadTickOutcome
RemediationRequired
ResealRequired
```

`next_dispatch` returns offline/idle/pause or claims one exact-owner pending sealed attempt. The future emitter resolves the opaque ID inside protection and sends the concrete `SealedTraceUpload` with the captured concrete session. Wave 2 never reads or reconstructs its body.

- [ ] **Step 3: Validate ACK completely before mutation**

For status 200 require `ok=true` and all accepted/rejected collections. Reject the entire ACK and mutate nothing if accepted or rejected contains a duplicate, accepted/rejected overlap, unknown ID, wrong entity, ID outside the attempt, or an unknown rejected code. Only after full validation call revision-CAS accepted deletion. Reschedule/reseal or remediation applies only to snapshot revisions still current; accepted parent items never remove children.

- [ ] **Step 4: Implement outcome semantics**

- Offline: zero claim and zero attempt change.
- Offline still runs retention expiry/tombstone maintenance before returning.
- Transport, 429, 5xx: retire used attempt, CAS-mark current revisions `NeedsSeal`, and apply `min(1s * 2^attempt, 5min) + injected bounded jitter`.
- 200 rejected/omitted: accepted-only CAS deletion; retire used attempt; current rejected/omitted revisions require a fresh seal without consuming retryable-failure attempt count.
- `credential_rejected`: delete no affected ID; return stable nonempty remediation keys and CAS-mark only current revisions. Future protection registers same IDs/entity/trace/parent at revision+1 in a new sealed attempt; empty results or ID drift fail.
- Durable remediation instructions are enumerable and replayable after restart until protection registers the replacement attempt.
- 422: delete zero; credential details may request remediation; incomplete evidence stays queued for a fresh seal.
- 401: retire used attempt, retain evidence, and persistently pause the captured owner generation; a new generation is a different scope.
- 426: retain and return update-required without retry.
- 400/403/409/413 or malformed response: retain and fail closed for manual intervention.

- [ ] **Step 5: Run focused GREEN**

```powershell
cargo test -p nwflash-infrastructure trace_uploader::tests --lib
```

Expected: all uploader tests pass. Main agent stages only `trace_uploader.rs` for `feat(trace): add attempt-bound uploader state machine`.

### Task 3: Shared wiring and gates

**Files:**
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/src/lib.rs`
- Do not modify: Wave 1 domain/protection files, workspace/application/tauri integration, `Cargo.lock`

- [ ] **Step 1: Export safe modules only**

Add the modules while keeping construction, opaque IDs, attempt manifests, owner hashes, and handles crate-private until the concrete Wave 1 bridge lands.

- [ ] **Step 2: Run focused gates only**

```powershell
cargo fmt --all -- --check
cargo test -p nwflash-infrastructure trace_spool::tests --lib
cargo test -p nwflash-infrastructure trace_uploader::tests --lib
cargo test -p nwflash-infrastructure
cargo clippy -p nwflash-infrastructure --all-targets -- -D warnings
```

Do not run workspace-wide or release builds. Verify `Cargo.lock` byte hash remains unchanged and `git diff --check` is clean.

- [ ] **Step 3: Independent spec and security review**

Reject the implementation if infrastructure contains payload/body/stream bytes, raw getters, serialization/redaction traits, reusable sealed attempts, response-time scope selection, non-CAS mutations, cascading parent ACK, or a tombstoned trace revival path.

- [ ] **Step 4: Commit wiring/review fixes and clean only this target after evidence**

Commit shared wiring as `test(trace): close opaque uploader safety gates`. After all commit/review evidence is captured, report the resolved absolute target path and size. Only then run `cargo clean --manifest-path <this-worktree>/src/Nwflash.Desktop/src-tauri/Cargo.toml`; never clean another worktree.

## Self-review

- Infrastructure owns metadata, never trace content or redaction proof.
- Every HTTP use consumes one protection-sealed attempt; retries require a fresh protection seal and wire upload ID.
- ACK, retry, pause, and remediation are bound to the persisted original owner and item revisions.
- Accepted-only means exact CAS deletion; parent acceptance never cascades.
- Seven-day loss is durable before removal and permanently tombstones owner+trace.
- No deployment, integration merge, Wave1 edit, workspace build, release build, or `Cargo.lock` mutation is authorized.
