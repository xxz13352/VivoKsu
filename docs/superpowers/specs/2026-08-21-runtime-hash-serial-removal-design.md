# Runtime Hash and Serial Binding Removal Design

**Date:** 2026-08-21

## Goal

Remove runtime image/artifact/OTA hash gates and cross-step phone serial bindings from NWFlash, per the explicit product decision, while preserving immediate command targeting, path/format/size checks, ownership cleanup, opaque IDs, and release/resource integrity.

## Scope

Remove:

- ROOT source-image and patched-artifact SHA-256/fingerprint storage, recomputation, rejection paths, and tests.
- Online Safe Flash OTA SHA-256 catalog/download verification and its DTO/API/test paths.
- Cached/preflight/current serial comparisons and device-bound capabilities in ROOT, ROOT OTA, Safe Flash, and Quick Flash execution flows.

Retain:

- `DeviceRuntime` serial discovery/display and the serial passed to the immediate ADB/Fastboot command being constructed.
- Single-current-device / multiple-device rejection.
- Image existence, extension, non-empty, partition, path, capacity, staging, cancellation, and first-failure protections.
- Release, installer, platform-tool, payload-dumper, scrcpy, manager APK, and resource-download integrity workflows.
- Session epoch invalidation, opaque IDs, owned staging cleanup, and one-shot plan semantics; these are session lifetime controls, not phone serial binding.

## Design

### ROOT and ROOT OTA

`RootImageSelection` and `RootPatchedArtifact` retain only opaque ID, epoch, image metadata, partition, and owned staging metadata. They do not retain a content digest or a phone serial. Lookup/consumption checks opaque ID and session epoch only.

ROOT patching resolves the current ADB/Fastboot serial immediately before the relevant command group, but does not compare it to a serial captured during selection or preflight. Automatic ROOT no longer rejects when a serial changes between stages; each stage derives its current target when building its immediate command.

`ResolvedRootOta` retains URL/name/PD/version/epoch but not serial. Extraction uses the currently available ADB target when required, without comparing it to a cached or post-extraction serial.

### Safe Flash and online OTA

`SafeFlashBuildOptions` and `SafeFlashExecutionRequest` no longer use a preflight serial as a binding check. The app resolves one current ADB/Fastboot target when it starts the immediate execution transition; fastbootd waiting accepts the sole discovered fastboot device and rejects multiple devices, not a serial mismatch.

Online OTA catalog SHA-256 is removed from runtime validation. Catalog/HEAD length, response byte ceilings, disk capacity, staging, cancellation, and publish cleanup remain. `OtaExpectedIntegrity` becomes a length-only runtime descriptor or equivalent expected-length argument; hash parsing, hashing, and mismatch gates are removed.

### Quick Flash

Plans may retain a serial as a transient command-building field for compatibility, but every execution entry point overwrites it with the current unique transport serial immediately before building commands. No execution rejects merely because a prepared/preview plan had another serial. Post-flash slot switch and reboot use that freshly resolved serial.

## Tests

- Replaced ROOT image/artifact bytes no longer invalidate opaque lookup or prepared-plan consumption; opaque ID/session epoch behavior remains tested.
- ROOT image/artifact and ROOT OTA state contain no device serial field and no serial mismatch error path.
- A Safe Flash/Quick Flash plan prepared on serial A executes against current sole serial B; multiple current devices still reject.
- ADB-to-fastbootd transition may proceed when the sole fastboot serial differs from the earlier ADB serial.
- Equal-length OTA bytes with a catalog SHA mismatch are accepted; short/oversized response, cancellation, destination preservation, and staging cleanup remain covered.
- Release/resource integrity tests remain unchanged.

## Non-goals

- Removing release or component SHA-256 verification.
- Removing immediate command targeting serials.
- Removing session capability epoch lifetime control.
- Changing generic UI confirmation behavior, external command cancellation, or device multi-connect rejection.
