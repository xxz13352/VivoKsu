# Visual Partition Flashing Design

## Goal

Add a visual partition workspace to VivoKsu. It reads every partition exposed by the connected device and supports partition backup, image writing, and partition erase through either Fastboot or ADB Root.

The feature does not parse or execute BAT/XML flash scripts. Image selection accepts `.img` and `.bin` files without filename restrictions.

## Confirmed UI

Keep the existing light white/teal WPF style and application shell.

- Center: searchable partition table with selection, size, slot, image path, state, and row action.
- Top: `Automatic / ADB Root / Fastboot` segmented transport selector and partition refresh.
- Bottom: backup, write, erase, stop, aggregate progress, speed, and elapsed time.
- Right: reuse the existing unified operation log.
- Lower left: continue showing the current device/task state.
- High-risk, active-slot, and mounted partitions are visually marked but remain operable.

The approved visual companion mockup is stored under `.superpowers/brainstorm/visual-flash-1786455597/`.

## Architecture

Use one shared workspace and two transport implementations:

- `PartitionWorkspaceViewModel` owns table state, filtering, selection, and image mapping.
- `IPartitionTransport` defines partition discovery, backup, write, erase, and capability checks.
- `AdbRootPartitionTransport` implements Root block-device operations.
- `FastbootPartitionTransport` implements fastboot-rs operations.
- `PartitionExecutionService` freezes and executes an immutable task list through `OperationCoordinator`.
- `PartitionRiskPolicy` supplies warnings only; it never disables an operation selected by the user.

UI code does not construct ADB or Fastboot commands directly.

## Partition Discovery

Automatic mode follows the current connection: an ADB device with UID 0 uses ADB Root, while a Fastboot device uses Fastboot. Manual selection restricts discovery and execution to the selected transport.

ADB Root scans available `/dev/block/*/by-name` locations, resolves duplicate links, and reads block size, slot, mount, and active-slot metadata. Fastboot parses `getvar all`, including `partition-size:*`, `partition-type:*`, and slot metadata.

Every refresh creates a new partition snapshot. Stale rows from an earlier device or transport are not retained.

## Image Mapping

Manual selection requires only an existing `.img` or `.bin` file. Directory mapping first matches exact partition names such as `boot_a.img`; a slotless name such as `boot.img` maps to the active slot by default and can be changed manually.

Raw files use their file length for capacity checks. Android Sparse images use their expanded size. Unknown capacity and oversized images produce warnings but do not block execution.

## Operations

Backup, write, and erase are separate queue types. Confirmation freezes the device serial, transport, partition path/name, image or output path, and execution order.

Tasks run sequentially through `OperationCoordinator`, which pauses normal device polling. The first failure stops the remaining queue. A stop request completes the current native partition command and cancels the remaining tasks.

ADB Root uses streamed transfer instead of staging a full partition in phone storage. Backups write to a local `.partial` file and rename it only after success. Writes and erases are never retried automatically. No SHA-256 verification system is introduced.

Fastboot uses the bundled fastboot-rs backend. Backup is available only when the connected device supports Fastboot fetch; write and erase use native Fastboot commands.

All discovered partitions remain writable and erasable after confirmation, including mounted, active-slot, and high-risk partitions. Risk metadata affects labels and confirmation text only.

## Progress And Errors

Each row exposes waiting, running, succeeded, failed, and canceled states. The footer reports current bytes, total queue progress, speed, and elapsed time when the native transport provides byte progress; otherwise it uses an indeterminate state.

Before each task, the service rechecks the device serial and transport. ADB Root also resolves the partition path again. Disconnects, Root loss, path changes, and native command errors stop the queue and are written to the unified log with partition, stage, and native error details.

Failed or canceled backups remove their `.partial` output.

## Execution Phases

1. Add shared partition models, Fastboot `getvar all` parsing, ADB Root discovery, and focused parser tests.
2. Extend the bundled Rust/C# boundary for Root streaming, Fastboot erase/fetch, structured errors, and progress reporting.
3. Implement immutable operation planning and sequential execution through `OperationCoordinator`.
4. Replace the current line-flash placeholder with the approved dense partition workspace.
5. Run focused tests, the full solution regression, Release build/publish, visual checks, and a controlled hardware smoke test.

## Focused Verification

- Both transports return a stable, deduplicated full partition table.
- `.img/.bin` files map without filename enforcement.
- Mounted and high-risk rows still generate write/erase tasks.
- Confirmed task snapshots cannot be changed by later UI edits.
- Failure and cancellation stop the remaining queue and clean partial backups.
- UI remains usable at the existing minimum window size and keeps the right log fixed.
- Hardware smoke tests cover discovery, one small backup, one controlled write, erase, disconnect, and stop behavior.

## Non-Goals

- No BAT or XML parsing/execution.
- No Qualcomm EDL/Sahara/Firehose transport.
- No automatic reboot or slot switch in the first version.
- No mixed backup/write/erase operations in one queue.
- No automatic retry or rollback of completed partition writes.
- No SHA-256 verification workflow.
