# 官方 KernelSU Vendor Boot 实施计划

> **For agentic workers:** Execute task-by-task with TDD checkpoints.

**Goal:** 将 Vivo 专用 ROOT 页改为两种互斥 KSU 方案，并支持官版 KernelSU 的 init_boot 与 vendor_boot 设备端处理。

**Architecture:** 资源服务持有三份固定 APK/二进制规格；ROOT ViewModel 只负责选择和门禁；设备端服务负责 `libksud.so` 修补和 `magiskboot.so` 的 vendor ramdisk 处理。

**Tech Stack:** .NET 8 WPF, CommunityToolkit.Mvvm, xUnit, ADB via FastbootRsBackend.

## Global Constraints

- 删除所有 `ksud.exe` / 外部修补器支持。
- 官方 KernelSU 必须同时选择 `init_boot` 和 `vendor_boot`。
- Vendor boot 仅处理脚本定义的官核 `lib/modules` 或检测到的 GKI `lib/modules/<release>-gki`。
- 资源与输入校验失败时禁止推送、修补、刷写。

### Task 1: 固定资源与方案模型

**Files:**
- Modify: `src/VivoKsu.App/Services/VivoRootResourceService.cs`
- Create: `tests/VivoKsu.App.Tests/OfficialKernelSuResourceTests.cs`
- Create: `src/VivoKsu.App/apk/KernelSU.apk`
- Create: `src/VivoKsu.App/root-tools/magiskboot.so`

- [ ] Write failing tests for official APK and magiskboot fixed metadata plus libksud extraction.
- [ ] Run the focused tests and confirm missing interfaces/resources.
- [ ] Add immutable specs, hash verification, and atomic extraction.
- [ ] Run focused tests green.

### Task 2: Vendor boot device processor

**Files:**
- Create: `src/VivoKsu.App/Services/VivoVendorBootProcessor.cs`
- Create: `tests/VivoKsu.App.Tests/VivoVendorBootProcessorTests.cs`

- [ ] Write failing tests for official/GKI command paths and cleanup.
- [ ] Run focused tests to prove red state.
- [ ] Implement push, unpack, path validation, modules file edits, repack, pull, output verification, cleanup.
- [ ] Run focused tests green.

### Task 3: ViewModel and UI migration

**Files:**
- Modify: `src/VivoKsu.App/ViewModels/RootViewModel.cs`
- Modify: `src/VivoKsu.App/MainWindow.xaml`
- Modify: `src/VivoKsu.App/MainWindow.xaml.cs`
- Modify: `tests/VivoKsu.App.Tests/RootViewModelTests.cs`

- [ ] Write failing tests for radio options and official KSU dual-image gating.
- [ ] Remove external patcher dependencies and implement official KSU orchestration.
- [ ] Rename visible page copy to Vivo 专用 and add the required controls.
- [ ] Run Root tests green.

### Task 4: Remove external ksud support and verify release

**Files:**
- Delete: `src/VivoKsu.App/Services/KsuPatchService.cs`
- Modify: `src/VivoKsu.App/Services/ToolPathPreferences.cs`
- Modify: external-patcher tests and project references

- [ ] Remove source, preference, UI and test references to `ksud.exe`.
- [ ] Run full Release test suite, build, publish, archive hash verification and startup smoke test.
