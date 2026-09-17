# Session Capability Revocation Design

**Date:** 2026-08-21

## Goal

Prevent a capability, artifact, selected image, OTA URL, or owned staging root created under one authenticated session from being consumed or republished after logout, session stop, or a later login.

## Problem

Current teardown removes the bearer token and, on session stop only, clears ROOT OTA state. Safe Flash, ROOT images/patch artifacts, firmware artifacts/extractions, and Quick Flash preflights survive. A naïve `OperationCoordinator::is_busy()` check is unsafe because admission authorization precedes the busy flag, several producers publish after the coordinator releases its permit, and ROOT selection/OTA checking are not coordinated at all.

## Selected approach

Use two complementary controls:

1. A session capability epoch held by Tauri state. A lease captured by a producer is valid only for its active epoch. Producers publish a result through a mutex-protected commit; consumers verify the lease before using an opaque capability. Invalidating the epoch and clearing runtimes use the same mutex, so late work cannot repopulate a cleared runtime.
2. An `OperationCoordinator` idle lease using the coordinator's existing single-operation semaphore. Logout/session stop hold that permit across invalidation and cleanup, closing the admission/check race. If another operation is active or authorizing, teardown fails closed with an in-progress error rather than cancelling a flash or deleting a staging directory in use.

## Lifecycle

```text
session_start succeeds
  -> activate a new capability epoch

sensitive producer begins
  -> capture epoch lease
  -> work outside locks
  -> commit result only if lease is still current

logout / session_stop
  -> acquire coordinator idle lease or return busy error
  -> invalidate epoch under capability mutex
  -> clear all runtimes and collect only Rust-owned staging roots
  -> remove collected roots after runtime locks are released
  -> stop lifecycle / clear token / flush usage

late producer completes
  -> commit rejects stale lease and cleans only its newly-owned staging
```

## Runtime scope

The central Tauri revocation method clears:

- `RootImageRuntime` — invalidate IDs, never delete selected user files.
- `RootPatchedArtifactRuntime` — invalidate IDs/prepared flash and delete only owned patch staging.
- `RootOtaRuntime` — existing resolved URL/staging cleanup.
- `FirmwareArtifactRuntime` and `FirmwareExtractionRuntime` — invalidate IDs and delete only their internal staging roots.
- `PayloadInspectionRuntime` and `RemoteFirmwareInspectionRuntime` — invalidate inspection IDs/stores.
- `SafeFlashRuntime` — remove pending preflight only when not executing, then delete only `source.staging_root`.
- `PreparedFirmwareArtifactRuntime` and `PreparedDualSlotRuntime` — invalidate one-shot plans without deleting user image paths.

Every producer that persists capability state captures a lease at its operation/request boundary and commits through the scope. Every capability consumer validates its lease before lookup/take.

## Public behavior

- `auth_logout` becomes async so it can acquire the idle lease and revoke state before clearing the token.
- `session_stop` returns the same in-progress failure if a controlled operation is running or authorizing; it does not cancel an active flash.
- After revocation, callers receive existing stale/preflight/session errors and must repeat inspection/preparation.
- Cleanup failure never preserves capability access. Owned staging removal is best-effort and is logged/surfaced only through the existing error boundary where appropriate.

## Tests

- Holding an idle lease blocks new operation admission, including the authorization window.
- Epoch unit tests prove stale producer commit and stale consumer access both fail after invalidation/re-activation.
- Each runtime clear invalidates IDs; tests distinguish owned staging removal from external user files.
- A delayed ROOT selection, ROOT OTA check, Safe Flash preparation, firmware extraction, and Quick Flash preparation cannot publish after revocation.
- Login/session command tests prove both logout and session stop use one revoke path, refuse while busy, and leave no previous-session capability usable.

## Non-goals

- Mid-flash cancellation policy.
- Generic browser-path Quick Flash confirmation capability redesign.
- Path-handle/snapshot elimination of the final external-tool open TOCTOU.
- Remote URL/redirect trust policy.
