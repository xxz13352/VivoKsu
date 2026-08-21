# ROOT Patched Artifact Integrity Design

**Date:** 2026-08-21

## Goal

Ensure ROOT patched-image capabilities cannot cause bytes different from the verified patched artifact to enter manual or automatic flash preparation.

## Context

`RootImageRuntime` already fingerprints selected source images. `RootPatchedArtifactRuntime` currently binds a patched artifact to an opaque ID, partition, device serial, optional owned staging root, and path/size, but not to its bytes. A same-size replacement of the patched file can therefore pass its current capability checks.

The affected execution paths are all rooted in `RootPatchedArtifactRuntime`:

- manual ROOT flash plan creation and prepared-plan consumption;
- automatic ROOT source assembly before Safe Flash;
- device-bound artifact lookup.

## Options considered

1. **Store SHA-256 with each patched artifact and recheck at every runtime lookup** — recommended. It matches the existing source-image pattern, preserves opaque IDs, and covers manual and automatic flows at their shared lookup boundary.
2. Copy every patched artifact into a new immutable snapshot before flashing. This eliminates the path mutation window but doubles large image storage and changes staging ownership/cleanup behavior.
3. Redesign all Quick Flash/Safe Flash artifact paths around generic Rust-owned confirmation capabilities. This is broader UI/API work and does not need to block the ROOT-specific correction.

## Selected design

Add a SHA-256 fingerprint to `RootPatchedArtifact` and make production registration require it.

- Compute the digest in the existing `spawn_blocking` validation work that already validates a newly patched output, before it is published to the runtime.
- Store the digest together with the opaque artifact record without exposing it in DTOs.
- Reuse the existing chunked SHA-256 implementation pattern; never hash while holding the runtime mutex.
- `get` and `get_for_device` verify current bytes against the stored digest before returning the artifact. This automatically protects manual plan creation, prepared-plan consumption, and automatic ROOT source assembly.
- Test-only helpers may construct synthetic artifacts without hashes, but production registration has no unverified path.
- On failed validation/fingerprinting, preserve the previously published artifact and remove only the newly created owned staging root using current cleanup paths.
- When superseding an owned artifact, take prior state while locked, then delete the prior owned staging root after releasing the lock.

## Error and lifecycle behavior

Changed/missing/unreadable artifact bytes return a localized invalid-operation error and do not create or consume a flash plan. Device-serial validation remains layered on top of byte verification.

This design has the standard path-level TOCTOU boundary between the final digest check and a later external fastboot open. Closing that boundary needs a handle/snapshot-based process-executor redesign and is outside this focused correction.

## Tests

- Register a verified temporary patched image, replace it with different bytes of the same length, and verify manual lookup rejects it before plan generation.
- Prepare a manual flash plan, replace same-size bytes, and verify prepared-plan consumption rejects it before execution.
- Verify automatic ROOT source assembly rejects a changed patched artifact.
- Verify unchanged bytes continue to yield the expected plan/source.
- Verify a failed replacement/fingerprint keeps the prior artifact/preflight valid and removes only the failed candidate's owned staging root.

## Non-goals

- Generic browser-path Quick Flash confirmation design.
- Safe Flash multi-partition content fingerprints.
- Session-generation cleanup for late capability publication after logout/session stop.
- In-flight fastboot cancellation policy or executor-level file-handle snapshots.
