# VMProtect release policy

This directory intentionally contains no VMProtect project export. VMProtect
Lite project data binds to operator-controlled paths and remains outside the
repository together with the SDK, license, compiler logs, and vendor binaries.

The release scripts prepare an immutable EXE/PDB/MAP handoff and stop with
`HANDOFF_REQUIRED`. An operator uses Lite GUI to protect the exact copied input
into the distinct path recorded by `prepared.json`, saves the compiler log, and
records the required marker/options review. Only `accepted.json` can authorize
signing and NSIS packaging.

`accepted.json` is a link in the evidence chain, not a bearer authorization:
finalize/signing replays the prepared-sidecar hashes, input layout/imports,
marker review, compiler log, protected-output hash/import removal, and isolated
VMProtect protected/CRC probe before any signing copy is used.

Run every VMP/protected-release script with PowerShell 7.4+ via `pwsh`; Windows
PowerShell 5.1 is intentionally rejected by the scripts' `#requires` boundary.

Only the eight named Rust protection leaves may be selected. The sixth source
contract is `NWFlash.TraceCredentialSentinel` in Ultra mode. Each concrete
bounded upload produced by a real `TraceOutputSession` receives an opaque
safe-Rust receipt bound to its ID and canonical already-redacted body digest;
receiptless uploads are rejected before public wire emission or producer sink
registration. The leaf itself sees only fixed-size identities, counts, and risk
state. Tauri/WebView, Tokio and async state machines, raw or redacted trace text,
serialization, spooling/HTTP/disk/chunking, device process control, downloads,
extraction, firmware writes, and third-party implementations remain outside
VMProtect regions.

The 2026-09-08 gate-chain audit added two leaves that close the highest-value
outside-the-circle patch targets found by the tamper review:

- `NWFlash.OperationAdmission` (symbol
  `nwflash_protection_requires_protected_recheck`, Ultra): the high-risk
  operation classification table that decides which operation kinds must pass
  the signed-lease recheck. Previously a one-line patch on the unprotected
  local classifier could route flash/write operations around the admission
  leaf.
- `NWFlash.TerminalExit` (symbol
  `nwflash_protection_terminate_process`, Ultra): the authoritative
  synchronous process terminator. The exit supervisor's production terminator
  and the Tauri event-loop return path both end here, so patching the
  outside-the-circle request()/worker dispatch cannot leave a
  terminated-for-integrity process alive.

The eight-leaf source/API gate and fixtures do not prove that a protected PE
contains these regions or that VMProtect Lite has run. Tauri production wrappers
still select the discarding observer instead of the `nwflash-windows`
observation interface, and the durable spool lacks a concrete adapter to the
producer facade. The final release must integrate those adapters, re-audit the
sensitive surface, and rerun source reachability, MAP/dumpbin, unchanged
eight-import SDK, compiler-log, marker-review, runtime protected/CRC, signing,
package, and installed smoke gates. Until then this source gate does not
authorize a Plan C release.
