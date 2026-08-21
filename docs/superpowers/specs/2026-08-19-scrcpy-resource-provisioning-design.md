# scrcpy Resource Provisioning Design

## Goal

Keep scrcpy outside the user-selected file workflow. A release may ship scrcpy under its bundled resources, while installations without that resource use the existing resource-installation flow to download it automatically.

## Design

- `ScrcpyProvisioner` owns all scrcpy source resolution.
- Source order is: valid bundled resource under the application resources directory, then a previously installed and verified package, then the official release metadata and download mirrors.
- The frontend exposes only the existing `resource_install(["scrcpy"])` action. It does not accept a scrcpy path, render a file picker, or invoke an arbitrary executable path.
- Release asset names are validated before they are used in staging or package paths. Downloaded archives remain protected by the GitHub SHA-256 digest and declared length.
- Published installations carry enough integrity metadata for later `is_installed` and `ensure_installed` checks to reject truncated or modified executables and trigger a fresh resource installation.
- Failed or canceled provisioning removes only the provisioner's owned staging directory; an existing valid installation remains untouched.

## Error Handling

Invalid release metadata, unsafe asset names, missing executables, digest mismatches, and corrupt installed packages are reported as resource-installation failures. The UI keeps its current generic resource error presentation and does not expose local paths or remote URLs as editable inputs.

## Testing

- Unit tests reject traversal and absolute release asset names.
- Unit tests reject a non-empty but modified published executable.
- Unit tests prove a valid installed package is reused without a download.
- Existing resource inventory and frontend tests prove scrcpy remains an installation item and no selection control is rendered.
