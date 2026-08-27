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

Only the five named Rust protection leaves may be selected. Tauri/WebView,
Tokio and async state machines, device process control, downloads, extraction,
firmware writes, and third-party code remain outside VMProtect regions.

Plan C trace transport is a separate future release gate. Once its synchronous
pre-spool/pre-HTTP/pre-chunk redaction and credential sentinel exist, the final
release must re-audit that sensitive surface, choose the leaf's VMP mode from
the implemented code, update the exact marker set, and rerun MAP/dumpbin/SDK and
package contracts. This five-leaf Task 8 result does not authorize Plan C.
