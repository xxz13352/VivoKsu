# External VMProtect SDK and Lite GUI handoff

VMProtect SDK files are external build inputs. Do not copy the SDK, SDK DLL,
license, VMProtect project, or machine-specific SDK path into this repository.

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
.\scripts\vmp\verify-sdk.ps1
cargo check --manifest-path .\src\Nwflash.Desktop\src-tauri\Cargo.toml `
  -p nwflash-protection --features vmp-sdk
.\scripts\vmp\verify-link-layout.ps1 `
  -SdkRoot $env:NWFLASH_VMP_SDK_ROOT
.\scripts\vmp\test-contracts.ps1 `
  -SdkRoot $env:NWFLASH_VMP_SDK_ROOT
```

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
| Image CRC and integrity decision dispatch | Virtualization |
| Build identity comparison | Mutation |

Do not expand marker ranges to Tauri/WebView entry points, Tokio or async state
machines, HTTP/TLS, adb/fastboot, drivers, child-process control, downloads,
decompression, firmware writes, or third-party code. Marker inputs are fixed
labels and never contain passwords, tokens, paths, URLs, or device serials.

Enable Memory Protection, Import Protection, and Packing for the protected
release. Virtual-machine denial remains disabled: debugger and VM detections
are classified telemetry signals only. Do not add a process exit or poll these
signals during a device operation.

VMProtect Lite uses a manual GUI handoff rather than repository automation.
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
- each of the five stable leaf symbols occurs exactly once in the MAP; and
- each physical leaf disassembly region contains exactly one expected Begin
  mode followed by exactly one `VMProtectEnd`.

These are unprotected pre-VMP layout artifacts. They must not be included in a
release package. Task 8 still owns the actual Lite GUI run, compiler-log review,
post-protection `VMProtectIsProtected`/CRC runtime checks, signing, packaging,
and final rejection of MAP/PDB/SDK files. Task 4 does not claim that the Lite
GUI has run or that post-protection CRC has been observed.
