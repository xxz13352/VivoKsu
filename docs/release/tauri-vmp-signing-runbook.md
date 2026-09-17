# Tauri VMP and signing release runbook

## Required controlled environment

The release host must provide `NWFLASH_VMP_PATH`, `NWFLASH_VMP_ARGUMENTS`, and `NWFLASH_CERT_THUMBPRINT`. Set `NWFLASH_SIGNTOOL_PATH` when the approved SignTool is not discoverable from the Windows SDK x64 installation. Do not store VMProtect licenses, certificate files, certificate passwords, production tokens, or absolute workstation paths in this repository.

`NWFLASH_VMP_ARGUMENTS` is a JSON array of strings supplied by the VMProtect owner. It must contain `{project}`, `{input}`, and `{output}` placeholders; the protection script rejects missing or non-string arguments before replacing the placeholders with the external project and staged EXE paths and executing the vendor console through an argument array. The produced output must be non-empty and have a different SHA-256 from the input.

## Required order

1. Run the Task 17 Rust, frontend, production-build, and UI acceptance gates against the unprotected EXE.
2. Run `Protect-NwflashRelease.ps1` against the staged EXE.
3. Run approved protected-EXE smoke tests.
4. Run `Sign-NwflashRelease.ps1` for the EXE.
5. Rebuild NSIS around the signed EXE and sign the final installer.
6. Run `Verify-ProtectedRelease.ps1` and archive its SHA-256 manifest and signature evidence.

`Publish-TauriRelease.ps1` is the protected release path and stops on every nonzero frontend build/test, Rust workspace test, unbundled Tauri build, native E2E, or NSIS bundle command before it can reach a later protection or signing step. `-DevelopmentUnsigned` is an explicit non-release bypass.

Only small Rust build identity, integrity, authorization-response binding, and local policy-dispatch functions can be selected for protection. Never protect the Tauri entry point, WebView, Tokio, event bridge, device process control, long download/extraction loops, or third-party code. Do not enable VM detection or deny execution in virtual machines.

## Cutover gate

The Tauri installer cannot become the default download until the Task 1 baseline, Task 16 visual certification, Task 17 mapping and fault-injection coverage, valid signatures, and approved protected-release device matrix are all recorded. Retain the WPF release for one additional release cycle as rollback.
