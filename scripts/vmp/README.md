# External VMProtect SDK and Lite GUI handoff

VMProtect SDK files are external build inputs. Do not copy the SDK, SDK DLL,
license, VMProtect project, or machine-specific SDK path into this repository.

All VMP, protected-release, and native-E2E PowerShell entry points require
PowerShell 7.4 or newer (`pwsh`, Core edition). Windows PowerShell 5.1 is not a
supported fallback: the scripts fail before execution through `#requires`
instead of reaching a later PS7-only API failure. Invoke entry points with an
explicit non-profile host, for example:

```text
pwsh -NoLogo -NoProfile -NonInteractive -File .\scripts\vmp\verify-sdk.ps1
```

## External SDK layout

Set `NWFLASH_VMP_SDK_ROOT` to a fully qualified package-root path that directly
contains:

```text
Include/C/VMProtectSDK.h
Lib/Windows/VMProtectSDK64.lib
Lib/Windows/VMProtectSDK64.dll
```

Validate the package read-only before an enabled build:

```powershell
$env:NWFLASH_VMP_SDK_ROOT = 'X:\external\VMProtect-package'
pwsh -NoLogo -NoProfile -NonInteractive -File .\scripts\vmp\verify-sdk.ps1
cargo check --manifest-path .\src\Nwflash.Desktop\src-tauri\Cargo.toml `
  -p nwflash-protection --features vmp-sdk
pwsh -NoLogo -NoProfile -NonInteractive -File .\scripts\vmp\verify-link-layout.ps1 `
  -SdkRoot $env:NWFLASH_VMP_SDK_ROOT
pwsh -NoLogo -NoProfile -NonInteractive -File .\scripts\vmp\test-contracts.ps1 `
  -SdkRoot $env:NWFLASH_VMP_SDK_ROOT
```

The protected release is pinned to the reviewed VMProtect Lite v3.10.4 Build
2668 x64 header, import library, and SDK DLL SHA-256 values. A structurally
similar or newer SDK is rejected until those release pins are deliberately
reviewed and updated.

Normal builds do not read `NWFLASH_VMP_SDK_ROOT`, inspect external paths, or
link VMProtect. Enabling `vmp-sdk` fails closed unless the target is Windows
x86_64 MSVC and the exact header/import library pass declaration and AMD64
COFF validation. The import library must map all eight consumed symbols to
`VMProtectSDK64.dll`. The read-only SDK verifier also locates x64 `dumpbin`
through `vswhere` and requires the actual DLL to export all eight functions.

The SDK DLL can be needed on `PATH` only when executing an unprotected binary
built with `vmp-sdk`, because that binary still imports SDK functions. It must
not be bundled or shipped after VMProtect has processed the executable. Release
artifact validation must reject `VMProtectSDK64.dll` and every other SDK file.

## Lite GUI marker modes

The Rust marker regions are explicit synchronous begin/body/end sequences with
fixed names. They do not depend on `Drop` or unwind cleanup, which matches the
`panic=abort` protected release. Configure the VMProtect Lite GUI to preserve
this intent:

| Boundary | Mode |
| --- | --- |
| Login lease acceptance | Ultra |
| Heartbeat lease classification | Virtualization |
| Local operation admission | Ultra |
| Protected operation recheck classification | Ultra |
| Image CRC and integrity decision dispatch | Virtualization |
| Build identity comparison | Mutation |
| Trace credential sentinel | Ultra |
| Terminal process exit | Ultra |

Do not expand marker ranges to Tauri/WebView entry points, Tokio or async state
machines, HTTP/TLS, adb/fastboot, drivers, child-process control, downloads,
decompression, firmware writes, or third-party code. Marker inputs are fixed
labels and never contain passwords, tokens, paths, URLs, or device serials.

## Plan C trace-redaction release gate

The source/API gate contains the sixth synchronous leaf,
`nwflash_protection_trace_credential_sentinel`, named
`NWFlash.TraceCredentialSentinel` and reviewed for Ultra mode. A real
`TraceOutputSession` forms each concrete bounded upload first, hashes its
canonical already-redacted body outside the marker, and synchronously asks the
leaf to mint an opaque safe-Rust receipt bound to that upload ID, digest, and
length. Receiptless or mismatched uploads cannot emit a public wire body, pass
the producer facade, or enter its sink trait.

The leaf input contains only fixed-size identities, counts, and risk state. Its
region excludes raw or redacted trace text, JSON/serialization, spooling, HTTP,
disk, chunking, async/Tokio work, and third-party implementations. This is
source/API gate evidence only: Tauri production wrappers still select the
discarding observer instead of the `nwflash-windows` observation interface,
and the durable spool has no concrete adapter to this producer facade.
It also does not prove that a release PE contains the physical leaf or that
VMProtect Lite has processed it. Plan C may proceed to a protected release only
after that adapter is integrated and a fresh source-reachability review,
eight-symbol MAP/dumpbin layout, unchanged eight-import SDK contract,
compiler-log/marker review, protected runtime probe, CRC, signing, package, and
installed smoke gates all pass. A PowerShell fixture pass alone is not release
authorization.

## 2026-09-08 gate-chain audit leaves

The tamper-bypass review found that the six original leaves protect the
verdicts (signature verification, lease validation, admission computation) but
not the wiring that decides whether a verdict is consulted or executed. Two
leaves were added:

- `nwflash_protection_requires_protected_recheck` (`NWFlash.OperationAdmission`
  marker, Ultra): owns the high-risk classification table. The unprotected
  dispatcher used to contain a plain `match` that could be patched to route
  flash/write operations around the admission leaf; the classification now
  lives only inside this leaf, and unknown wire indices classify as high-risk
  (fail-closed).
- `nwflash_protection_terminate_process` (`NWFlash.TerminalExit` marker,
  Ultra): the authoritative process terminator. The exit supervisor's
  production terminator and the Tauri event-loop return path both call this
  leaf, so patching the outside-the-circle exit-request plumbing cannot keep
  an integrity-terminated process alive.

Both leaves take only fixed-size inputs (a `u32` wire index; an exit code) and
never handle credentials, tokens, or trace text.

Enable Memory Protection, Import Protection, and Packing for the protected
release. Virtual-machine denial remains disabled: debugger and VM detections
are classified telemetry signals only. Do not add a process exit or poll these
signals during a device operation.

VMProtect Lite uses a manual GUI handoff rather than repository automation.
Automated `VMProtect_Con.exe` execution is disabled by the repository scripts.
Keep its project and license external, protect the prepared unsigned executable
into a new output file, confirm the output changed, and then continue with
post-VMP signing and installer creation. Never overwrite or ship the original
unprotected executable as the protected release.

## Pre-VMP link and physical-layout contract

`verify-link-layout.ps1` performs a full optimized link of the minimal
`vmp_link_probe` example; `cargo check` is not accepted as linker evidence. The
script enables `panic=abort` and debug information, emits an EXE, PDB, and MAP
under Cargo's ignored `target/release/examples` directory, and then uses x64
`dumpbin` plus the MAP to fail closed unless:

- the final PE has exactly one `VMProtectSDK64.dll` import block containing the
  eight required symbols and no additional VMProtect import;
- each of the eight stable leaf symbols occurs exactly once in the MAP; and
- each physical leaf disassembly region contains exactly one expected Begin
  mode followed by exactly one `VMProtectEnd`.

These are unprotected pre-VMP layout artifacts. They must not be included in a
release package. Task 8 still owns the actual Lite GUI run, compiler-log review,
post-protection `VMProtectIsProtected`/CRC runtime checks, signing, packaging,
and final rejection of MAP/PDB/SDK files. Task 4 does not claim that the Lite
GUI has run or that post-protection CRC has been observed.

`accepted.json` is not trusted merely because it is read-only. Every finalize
and EXE-signing entry point revalidates its hash-bound `prepared.json`, SDK and
link-layout sidecars, exact input EXE/PDB/MAP, marker review, compiler log,
distinct protected output, removed SDK imports, and isolated protected/CRC
runtime probe before allowing the signing copy to be created.
