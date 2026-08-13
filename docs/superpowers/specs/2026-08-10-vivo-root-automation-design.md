# Vivo ROOT Automation Design

## Goal

Make the Vivo ROOT page select the correct workflow for Vivo KSU or official KernelSU, default KMI to automatic detection, support manual KMI override, and complete the supported patch-and-flash sequence from one command.

## Workflow Rules

- Vivo KSU accepts only an `init_boot` image. `vendor_boot` selection, processing, result display, and flash actions are unavailable in this mode.
- Official KernelSU requires `init_boot` and `vendor_boot`, retaining the existing vendor processor and kernel-mode controls.
- The image chooser accepts `.img` and `.bin`; the selected file must still include the expected partition name.
- Automatic KMI is enabled by default. It maps the connected ADB device kernel release to a supported KMI. The operator can refresh detection with a command or turn it off and select a supported KMI manually.
- Full automation runs: preflight -> install manager -> patch init_boot -> patch vendor_boot when official -> reboot to bootloader -> wait for fastboot -> flash init_boot -> flash vendor_boot when official -> reboot system.
- Each stage updates the session status and shared operation log. A failed stage terminates the workflow and preserves generated images for manual recovery.

## Architecture

`RootViewModel` owns UI state, command availability, KMI choice, and orchestration. `VivoRootResourceService` provides deterministic KMI mapping; `VivoKsuDevicePatchService` and `VivoVendorBootProcessor` produce images; `QuickFlashService` gains a sequential root-flash operation with one final reboot. The existing normal quick-flash behavior stays unchanged.

## Validation

- Unit tests cover automatic/manual KMI resolution, Vivo-vs-official preflight, accepted `.bin` image inspection, and the full sequence's partition order/reboot behavior.
- The full test suite and Release build are required before delivery.
