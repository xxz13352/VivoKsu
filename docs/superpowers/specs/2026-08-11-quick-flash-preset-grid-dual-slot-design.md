# Quick Flash Preset Grid And Dual-Slot Design

## Goal

Replace the current single-partition quick-flash form with the compact preset layout from the supplied references. The page must support selecting and flashing multiple approved partitions, flashing one row independently, writing every selected partition to both A/B slots, optionally switching to the opposite active slot, optionally rebooting, and either waiting for or immediately requiring the selected Fastboot mode.

The existing light visual language, fixed navigation, lower-left device state, right-side unified log, shared `OperationCoordinator`, and bundled `fastboot-rs` backend remain unchanged.

## Chosen Approach

Use a table-driven preset collection. Each row is one approved partition with its own selected image and row commands. Batch flash consumes the selected rows and passes a normalized flash request list to one service method.

This is preferred over duplicated ViewModel property groups because selection, command state, logging, confirmation, and tests remain uniform. An arbitrary partition editor is intentionally excluded because quick flash remains a controlled whitelist rather than a second line-flash surface.

## Preset Partitions

The grid contains four fixed rows in the final order:

| Left column | Right column |
| --- | --- |
| `boot` | `init_boot` |
| `vendor_boot` | `lk` |

`QuickFlashPartition` is limited to these four values. The service remains the only place that maps enum values to Fastboot partition names.

## Layout

The existing page title and device status remain. The large split form is replaced by one compact panel:

1. A top action bar contains `开始刷入`, target-mode selection (`Fastboot` / `fastbootd`), and four independent checkbox controls:
   - `自动重启`, default on;
   - `等待 FB 设备`, default on;
   - `双刷入双槽`, default off;
   - `刷完切槽`, default off and disabled while dual-slot flashing is off.
2. A `预置分区刷入` body uses two equal columns with two stable rows.
3. Every row contains a fixed partition label, a read-only ellipsized image path, a `文件` button, and a `刷入` button.
4. `开始刷入` flashes every row with a selected image. A row `刷入` command flashes only that row using the same global options.
5. The existing confirmation surface remains but becomes compact and summarizes partition count, target mode, dual-slot state, slot-switch state, and reboot state.
6. `取消当前操作` replaces the start action while an operation is active.

The page uses the current white/teal/signal palette and existing 5-7 px radius system. The reference determines density and placement, not its purple accent. No nested cards, decorative gradients, or extra explanatory copy are added.

## ViewModel Model

Introduce a row model named `QuickFlashPresetItemViewModel`, with:

- `QuickFlashPartition Partition`;
- `string DisplayName`;
- `FlashImageInfo? SelectedImage`;
- derived path display and `HasImage` state.

`QuickFlashViewModel` exposes the ordered preset collection and parameterized browse/row-flash commands. It also exposes:

- `AutoReboot = true`;
- `WaitForDevice = true`;
- `FlashBothSlots = false`;
- `SwitchSlotAfterFlash = false`;
- the existing `SelectedTarget` for Fastboot versus fastbootd;
- a pending immutable flash request snapshot used by confirmation.

Turning off `FlashBothSlots` automatically turns off `SwitchSlotAfterFlash`. The switch-slot checkbox is disabled unless dual-slot flashing is enabled.

`PreparePatchedImage` remains compatible with ROOT and line-flash handoff. It places the supplied image into the matching preset row, selects Fastboot, and clears any stale confirmation.

## Flash Request And Data Flow

Both batch and row flashing produce an immutable list of `(Partition, Image)` requests before confirmation. Later UI selection changes cannot alter the confirmed operation.

The service receives the request list plus target and option values:

```text
requests
target mode
wait for device
flash both slots
switch slot after flash
auto reboot
```

The operation runs inside the shared `OperationCoordinator`, so device polling remains paused and all stages are written to the right-side correlated log.

## Device And Slot Preflight

Before the first write:

1. When `等待 FB 设备` is enabled, poll until a matching Fastboot or fastbootd device appears.
2. When it is disabled, perform one discovery and fail immediately if the selected mode is not ready.
3. When dual-slot flashing is enabled, query `has-slot:<partition>` for every selected partition. Every result must resolve to `yes` or `true`; otherwise abort before writing anything.
4. When slot switching is enabled, read and normalize `current-slot`. Only `a`, `_a`, `b`, and `_b` are accepted. Failure to determine the current slot aborts before writing anything.

These checks prevent a partially written queue caused by discovering an unsupported partition halfway through the operation.

## Flash Order

Normal mode flashes the approved base partition name exactly once:

```text
flash boot image.img
```

Dual-slot mode processes each selected partition completely before advancing:

```text
flash boot_a image.img
flash boot_b image.img
flash init_boot_a image.img
flash init_boot_b image.img
```

If any flash fails, stop immediately. Do not switch slots and do not reboot. The log names the partition and slot that failed; already completed device writes are not described as rolled back.

After all writes succeed, `刷完切槽` sets the opposite of the slot captured during preflight. For example, current `a` becomes active `b`. The operation then reboots only when `自动重启` is enabled.

## Native Boundary

The bundled Rust library already exports:

```text
fastboot_set_active(serial, slot)
```

Expose it through `FastbootRsNative`, `NativeFastbootRsApi`, `IFastbootRsNativeApi`, `FastbootRsApiWithPlatformDeviceDiscovery`, `PlatformToolsNativeApi`, and `FastbootRsBackend`.

The platform-tools fallback executes `fastboot -s <serial> set_active <slot>`. The combined implementation continues to use native `fastboot-rs` for real fastboot operations.

## Command And Error States

- Batch start is disabled when no preset row has an image, the session is busy, or a flash is already active.
- Row flash is disabled until that row has an image or while another operation is active.
- Browse remains available only while no flash is active.
- Confirmation cancellation changes no selected images or options.
- Operation cancellation stops at the next cancellable backend boundary and never continues into slot switching or reboot.
- Image selection still accepts only `.img` and `.bin`; filenames are unrestricted.
- Auto reboot off leaves the device in its current Fastboot mode after successful writes and optional slot switch.

## Testing

Add focused red-green coverage for:

1. The exact four-row preset order and default option values.
2. Batch flash including only rows with selected images.
3. Row flash using only its row while preserving global options.
4. Dual-slot requests ordered per partition as `_a` then `_b`.
5. Preflight rejection when any selected partition is not slot-aware, with zero writes.
6. Opposite-slot calculation and `set_active` ordering after all successful writes.
7. No slot switch and no reboot after a flash failure.
8. Auto reboot off producing no reboot request.
9. Wait disabled performing one discovery instead of polling.
10. ROOT `PreparePatchedImage` handoff populating the correct preset row.
11. Native and platform-tools `set_active` bindings.
12. Existing cancellation and coordinator tests remaining valid.

Run the focused quick-flash tests, the full solution tests, Release build, self-contained publish, startup smoke test, and a WPF screenshot inspection at the current desktop viewport.

## Non-Goals

- No arbitrary partition-name input.
- No automatic image discovery from a firmware package.
- No active-slot switch before flashing.
- No separate image per A and B slot; the same selected image is written to both.
- No changes to ROOT patch algorithms, file management, line-flash parsing, or the global log layout.

## Implementation Status (2026-08-11)

- Implemented the four-row `boot`, `init_boot`, `vendor_boot`, and `lk` preset grid and compact two-column WPF layout.
- Implemented batch and row-level immutable confirmation plans.
- Implemented Fastboot/Fastbootd matching, optional wait, dual-slot preflight, `_a` then `_b` writes, opposite-slot activation, and optional reboot.
- Exposed `fastboot_set_active` through native fastboot-rs, platform-tools fallback, composite API, and async backend.
- Preserved ROOT handoff and existing operation-coordinator cancellation behavior.
- Focused quick-flash/backend regression: 30/30 passed.
- Full solution regression: 149/149 passed.
- Release build: 0 warnings, 0 errors.
- Self-contained package: `artifacts/release/VivoKsu-win-x64/` and `artifacts/release/VivoKsu-win-x64.zip`.
- Visual evidence: `artifacts/verification/quick-flash-preset-grid.png` and minimum-size check `artifacts/verification/quick-flash-preset-grid-min.png`.
- Published EXE remained alive for more than three seconds during startup smoke verification.
