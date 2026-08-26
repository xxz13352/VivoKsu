# VMProtect release policy

This directory intentionally contains no VMProtect project export. VMProtect
Lite project data binds to operator-controlled paths and remains outside the
repository together with the SDK, license, compiler logs, and vendor binaries.

The release scripts prepare an immutable EXE/PDB/MAP handoff and stop with
`HANDOFF_REQUIRED`. An operator uses Lite GUI to protect the exact copied input
into the distinct path recorded by `prepared.json`, saves the compiler log, and
records the required marker/options review. Only `accepted.json` can authorize
signing and NSIS packaging.

Only the five named Rust protection leaves may be selected. Tauri/WebView,
Tokio and async state machines, device process control, downloads, extraction,
firmware writes, and third-party code remain outside VMProtect regions.
