# Vivo ROOT Automation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add default automatic KMI detection, manual KMI override, `.bin` image support, mode-specific vendor behavior, and an end-to-end ROOT automation command.

**Architecture:** Keep state and command orchestration in `RootViewModel`. Add reusable sequential-flash support to `QuickFlashService`, preserving existing one-image quick flash behavior. Use existing device patch services and session/log services for all device I/O and status reporting.

**Tech Stack:** .NET 8 WPF, CommunityToolkit.Mvvm, xUnit.

## Global Constraints

- No SHA-256 runtime validation or display.
- Vivo KSU uses `init_boot` only; official KernelSU uses `init_boot` and `vendor_boot`.
- Accepted image extensions are `.img` and `.bin`.
- Full automation reboots only after every required partition has flashed.

---

### Task 1: KMI mode and image validation

**Files:**
- Modify: `tests/VivoKsu.App.Tests/RootViewModelTests.cs`
- Modify: `tests/VivoKsu.App.Tests/QuickFlashServiceTests.cs`
- Modify: `src/VivoKsu.App/ViewModels/RootViewModel.cs`
- Modify: `src/VivoKsu.App/Services/QuickFlashService.cs`

**Interfaces:**
- Produces `RootViewModel.UseAutomaticKmi`, `RootViewModel.ResolveKmiCommand`, and `RootViewModel.EffectiveKmi`.
- Extends `QuickFlashService.InspectImageAsync` to accept `.img` and `.bin`.

- [ ] Write failing tests for automatic KMI default/manual override and `.bin` image inspection.
- [ ] Run focused tests and confirm the expected failures.
- [ ] Add minimal KMI state and image-extension validation.
- [ ] Run focused tests and confirm they pass.

### Task 2: ROOT workflow orchestration

**Files:**
- Modify: `tests/VivoKsu.App.Tests/RootViewModelTests.cs`
- Modify: `src/VivoKsu.App/ViewModels/RootViewModel.cs`
- Modify: `src/VivoKsu.App/Services/QuickFlashService.cs`

**Interfaces:**
- Produces `RootViewModel.RunAutomaticRootCommand`.
- Adds `QuickFlashService.FlashRootImagesAsync(session, images, target, token)`.

- [ ] Write failing tests for Vivo's single-partition sequence and official KernelSU's two-partition sequence.
- [ ] Run focused tests and confirm the expected failures.
- [ ] Implement ordered flashing with one final system reboot and stage-level logging.
- [ ] Run focused tests and confirm they pass.

### Task 3: WPF controls

**Files:**
- Modify: `src/VivoKsu.App/MainWindow.xaml`

**Interfaces:**
- Binds to the Task 1 and Task 2 view-model members.

- [ ] Replace the fixed KMI selector with automatic/manual controls and a refresh command.
- [ ] Keep vendor controls and vendor result/flash action visible only for official KernelSU.
- [ ] Add a single full-automation command while retaining individual recovery actions.
- [ ] Build the WPF project to confirm XAML bindings compile.

### Task 4: Verification

**Files:**
- Modify: none.

- [ ] Run `dotnet test VivoKsu.slnx --no-restore`.
- [ ] Run `dotnet build VivoKsu.slnx -c Release --no-restore`.
- [ ] Inspect the UI state in a locally launched build.
