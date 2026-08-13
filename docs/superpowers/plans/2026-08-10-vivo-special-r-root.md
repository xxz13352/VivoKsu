# Vivo 专用 R ROOT 迁移实施计划

> **For agentic workers:** Execute task-by-task with TDD checkpoints.

**Goal:** 将 `mtkbl` 的 KSU/Sukisu ROOT 资源与安装前置迁移到 VivoKsu WPF，并固定归类为 Vivo 专用 R。

**Architecture:** 新增独立资源服务承载固定 APK/KMI 元数据和 ZIP 校验；RootViewModel 负责选择、预检和 ADB 安装；镜像刷写复用现有 QuickFlashService。

**Tech Stack:** .NET 8 WPF, CommunityToolkit.Mvvm, xUnit, existing FastbootRsBackend.

## Global Constraints

- UI 布局保持现状，只修改 ROOT 页文案和必要控件。
- KMI allowlist 固定为 `android13-5.15`、`android14-6.1`、`android15-6.6`。
- APK、`libksud.so`、源镜像校验失败时禁止继续。
- `mtkbl` 项目不修改。

### Task 1: Resource catalog and verification

**Files:**
- Create: `src/VivoKsu.App/Services/VivoRootResourceService.cs`
- Create: `tests/VivoKsu.App.Tests/VivoRootResourceServiceTests.cs`
- Modify: `src/VivoKsu.App/VivoKsu.App.csproj`

- [ ] Write failing tests for KMI allowlist, kernel mapping, manager APK verification, and `libksud.so` extraction.
- [ ] Run the focused tests and confirm they fail because the service is absent.
- [ ] Implement immutable manager specs copied from `mtkbl`, ZIP entry validation, atomic extraction, and APK root resolution from `apk\`.
- [ ] Run the focused tests until green.

### Task 2: Root ViewModel integration

**Files:**
- Modify: `src/VivoKsu.App/ViewModels/RootViewModel.cs`
- Modify: `tests/VivoKsu.App.Tests/RootViewModelTests.cs`
- Modify: `src/VivoKsu.App/MainWindow.xaml.cs`

- [ ] Add manager/KMI collections, selected values, detected KMI, APK status, and install command.
- [ ] Add failing ViewModel tests for supported selections and ADB install gating.
- [ ] Implement minimal command flow: verify APK, install with `-r`, verify package, launch Activity, and log stages.
- [ ] Run Root tests and then the full test suite.

### Task 3: Vivo 专用 R copy and packaged assets

**Files:**
- Modify: `src/VivoKsu.App/MainWindow.xaml`
- Modify: `src/VivoKsu.App/Models/AppPage.cs` only if naming requires it.
- Create: `src/VivoKsu.App/apk/KSU.APK`
- Create: `src/VivoKsu.App/apk/Sukisu.APK`

- [ ] Rename visible ROOT navigation/page copy to “Vivo 专用 R”.
- [ ] Add manager/KMI controls and install button without changing the page shell.
- [ ] Copy and package the verified APKs from `mtkbl`.
- [ ] Run build and release publish.

### Task 4: Verification

- [ ] Run `dotnet test VivoKsu.slnx --configuration Release`.
- [ ] Run `dotnet publish src/VivoKsu.App/VivoKsu.App.csproj -c Release -r win-x64 --self-contained true`.
- [ ] Verify packaged APK hashes and launch the Release executable smoke test.
