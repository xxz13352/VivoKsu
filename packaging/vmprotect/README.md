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

Only the six named Rust protection leaves may be selected. The sixth source
contract is `NWFlash.TraceCredentialSentinel` in Ultra mode. Each concrete
bounded upload produced by a real `TraceOutputSession` receives an opaque
safe-Rust receipt bound to its ID and canonical already-redacted body digest;
receiptless uploads are rejected before public wire emission or producer sink
registration. The leaf itself sees only fixed-size identities, counts, and risk
state. Tauri/WebView, Tokio and async state machines, raw or redacted trace text,
serialization, spooling/HTTP/disk/chunking, device process control, downloads,
extraction, firmware writes, and third-party implementations remain outside
VMProtect regions.

The six-leaf source/API gate and fixtures do not prove that a protected PE
contains these regions or that VMProtect Lite has run. Tauri production wrappers
still select the discarding observer instead of the `nwflash-windows`
observation interface, and the durable spool lacks a concrete adapter to the
producer facade. The final release must integrate those adapters, re-audit the
sensitive surface, and rerun source reachability, MAP/dumpbin, unchanged
eight-import SDK, compiler-log, marker-review, runtime protected/CRC, signing,
package, and installed smoke gates. Until then this source gate does not
authorize a Plan C release.
