# VivoKsu Device Workbench Design

## Product Boundary

VivoKsu is a single-device Android maintenance workbench for Windows 10 and Windows 11. It uses the vendored `fastboot-rs` C ABI library for supported ADB/Fastboot operations and keeps every operation trace in one fixed right-side log pane.

Windows 7 support is intentionally removed. The application remains a C# WPF desktop program rather than being rewritten in C++.

## Technical Baseline

- Target: `.NET 8`, `net8.0-windows`, WPF, C#.
- MVVM: existing `CommunityToolkit.Mvvm` 8.4.0.
- UI control library: `HandyControl` 3.5.1. A clean .NET 8 WPF restore/build probe completes with zero warnings and zero errors.
- Native bridge: the existing `FastbootRsBackend` remains the only managed/native boundary for `platform-tools/fastboot.dll`.
- Icons: use the control library's icon facilities or a single compatible icon package, never manually drawn SVG icon buttons.
- Process integration: ADB-only features not exposed through `fastboot.dll`, including directory listing and screen mirroring, run through a typed `IProcessRunner` abstraction. `scrcpy` is managed as a child process with explicit start, stop, and exit events.

HandyControl is used for functional controls such as switches, dialogs, notifications, progress indicators, and file pickers. Its stock theme is not used as the product visual language.

## Visual System

The application adopts a precision-instrument workbench language:

- Graphite and titanium-gray surfaces establish the structural hierarchy.
- Signal orange denotes irreversible or mode-changing actions. It is reserved for active Fastboot/restart routes and warnings.
- Low-saturation aqua denotes a healthy, authorized device connection and verified results.
- Border radius is small (3-5px); cards are not used as page decoration.
- Layout uses clear rules, compact monospace labels for machine values, and plain Chinese labels for actions.
- No decorative gradients, glass panels, glow effects, oversized marketing text, or animated background elements.

The three permanent regions are:

1. Top application bar: product identity, device refresh, settings, and connection indicator.
2. Left navigation rail: page navigation plus a bottom-anchored device state panel.
3. Right operation-log pane: retained on every tool page, with time, severity, command output, and clear/filter controls.

The main content column is the only region that changes between pages. At narrower desktop widths the right log pane can collapse behind a log toggle; the device state remains anchored to the lower left rail.

## Shared State and Logging

`DeviceSessionViewModel` owns the selected device and publishes model, codename, serial, ADB/Fastboot transport, Android version, active slot, battery, bootloader status, verified-boot state, kernel release, and build/version fields.

`OperationCoordinator` serializes device-changing operations. It exposes `Idle`, `Discovering`, `Rebooting`, `Installing`, `Transferring`, `Flashing`, `Mirroring`, `Completed`, `Canceled`, and `Failed` states. The lower-left status panel displays the current state and a concise activity string.

`OperationLogViewModel` is shared by all pages. It appends timestamped entries with `Info`, `Success`, `Warning`, and `Error` severities, command output, and operation correlation IDs. Pages never create their own detached log surface.

## Pages

### Device Overview

This page has exactly two functional sections:

1. Device information: device identity plus a structured property matrix for model, codename, serial, Android/OriginOS build, kernel, active slot, bootloader, verified boot, and USB authorization.
2. Restart modes: Normal system restart, Bootloader restart, and Fastboot restart. There are no quick-flash, file, ROOT, projection, or line-flash controls on this page.

Unavailable values display `Not available` rather than stale data. Actions are disabled until the connected transport supports them.

### Quick Flash

The quick-flash page exposes only `boot`, `init_boot`, and `vendor_boot` presets. The user chooses one image, sees its size and SHA-256, then chooses one wait mode: `Fastboot` or `fastbootd`. The workflow waits for the expected device mode, flashes the approved partition, records raw output, and automatically requests a normal reboot after a successful flash. The user receives a confirmation dialog before starting a write operation.

### ADB Mirror

The page manages a `scrcpy` process. It has an enabled/disabled `Auto mirror` switch, `Start mirroring`, and `Stop mirroring` actions. When enabled, the session watcher starts mirroring after an authorized ADB device appears. If the mirror process exits while the device is still authorized, the watcher restarts it after a short bounded delay. A deliberate Stop suppresses automatic restart until the user starts it again or a new device session begins.

### ROOT Tools

This page is a planned-workflow screen. It visibly marks the feature as under development and lists the future KSU patch-and-flash and kernel-signature workflows. It does not expose incomplete destructive actions in the first implementation phase.

### File Manager and APK Install

The page is a split local/remote file manager. The remote side lists the active ADB device path; the local side uses the Windows file system. It supports directory navigation, upload, download, delete after confirmation, operation progress, and cancellation. APK install is integrated in the page as a dedicated file selection and install panel; it validates the `.apk` extension and writes the result to the global log.

### Line Flash

The page is also marked under development. Its initial display is a visual partition table populated from detected Fastboot variables and local image associations. The Vivo line-flash route is labelled planned and cannot execute any operation until it has a defined backend protocol.

## Service Boundaries

```text
Views / ViewModels
        |
DeviceSessionService ---- DeviceInfoService
        |                         |
OperationCoordinator ---------- FastbootRsBackend
        |                         |
        |                    IProcessRunner
        |                         |
OperationLogService ---- AdbFileService / MirrorService / QuickFlashService
```

- `DeviceSessionService` polls for device transport transitions and refreshes device properties when an authorized device changes.
- `QuickFlashService` validates a fixed partition name, waits for Fastboot state, flashes through the native backend, and requests reboot only after success.
- `MirrorService` owns the `scrcpy` lifetime, auto-mirror policy, and exit-restart guard.
- `AdbFileService` converts structured directory results into file entries and implements upload/download/delete.
- `IProcessRunner` is mockable so command construction, output handling, exit codes, cancellation, and mirror restart behavior can be tested without a device.

## Delivery Order

1. Introduce HandyControl, the redesigned three-column shell, fixed right log pane, fixed lower-left device status, and the device-overview/reboot page.
2. Add shared session discovery, complete device property collection, and state-driven command enablement.
3. Implement Quick Flash with the three approved partitions and automatic post-success reboot.
4. Implement ADB Mirror process control and auto-mirror lifecycle policy.
5. Implement File Manager plus integrated APK install.
6. Add planned ROOT and Line Flash pages with non-executable visual structures, then extend them only when their backend workflows are defined.

## Verification

- Build and unit-test the project after each delivery step.
- Add tests for property parsing, restart command selection, partition validation, wait-mode selection, SHA-256 generation, and flash/reboot state transitions.
- Add process-runner fixtures covering successful and failed `scrcpy` starts, deliberate stop, unexpected mirror exit, and authorized-device reconnects.
- Add file-service fixtures for directory parsing, upload/download command construction, APK validation, and cancellation.
- Perform visual checks at the existing minimum window size and a compact desktop width; verify all text remains visible and the log pane is reachable.

## Explicit Non-Goals for the First Increment

- Windows 7 support.
- Arbitrary partition flashing.
- Executable KSU, kernel-signature, or Vivo line-flash operations.
- Multiple-device simultaneous management.
