# External VMProtect SDK and Lite GUI handoff

VMProtect SDK files are external build inputs. Do not copy the SDK, SDK DLL,
license, VMProtect project, or machine-specific SDK path into this repository.

## External SDK layout

Set `NWFLASH_VMP_SDK_ROOT` to the package root that directly contains:

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
```

Normal builds do not read `NWFLASH_VMP_SDK_ROOT`, inspect external paths, or
link VMProtect. Enabling `vmp-sdk` fails closed unless the target is Windows
x86_64 MSVC and the exact header/import library pass declaration and AMD64
COFF validation.

The SDK DLL can be needed on `PATH` only when executing an unprotected binary
built with `vmp-sdk`, because that binary still imports SDK functions. It must
not be bundled or shipped after VMProtect has processed the executable. Release
artifact validation must reject `VMProtectSDK64.dll` and every other SDK file.

## Lite GUI marker modes

The Rust marker scopes are synchronous RAII guards with fixed names. Configure
the VMProtect Lite GUI to preserve this intent:

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
