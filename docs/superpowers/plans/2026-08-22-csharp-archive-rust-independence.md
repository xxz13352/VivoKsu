# C# Archive and Rust Independence Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Archive the complete C# desktop version while making the Rust/Tauri desktop client build, test, package, and update without active C# dependencies.

**Architecture:** The Tauri resource tree becomes the only active source of shipped binaries, and React owns its brand image. C# source, tests, WPF-only scripts, and historical migration documentation move beneath `archive/csharp/` with their existing relative layout intact, while Rust removes the C# migration-map gate and validates only its own contracts.

**Tech Stack:** Rust/Cargo, Tauri, React/Vite/Vitest, PowerShell release scripts, Git moves, .NET archive verification.

## Global Constraints

- Do not modify `cloudflare/**`.
- Do not change shipped resource bytes or their approved SHA-256 values.
- Use `archive/csharp/` for every moved C# application, bootstrapper, test, solution, WPF-only script, and historical WPF migration document.
- Keep the archived C# layout as `archive/csharp/{src,tests,scripts,docs}` so original relative project references remain valid.
- Active Rust/Tauri code, scripts, release manifests, and active documentation must not reference `src/VivoKsu.App`, `src/VivoKsu.Bootstrapper`, `tests/VivoKsu.App.Tests`, or root `VivoKsu.slnx`.
- Retain the legacy `Sukisu.APK` only in the C# archive because the active Rust release does not consume it.

---

### Task 1: Make Rust/Tauri the canonical resource owner

**Files:**
- Create: `src/Nwflash.Desktop/src/assets/logo.jpg`
- Modify: `src/Nwflash.Desktop/src/components/AppShell.tsx`
- Modify: `src/Nwflash.Desktop/src/components/LoginScreen.tsx`
- Modify: `packaging/release/tauri-resources.json`
- Modify: `scripts/Upload-Resources.ps1`

**Interfaces:**
- Consumes: the existing byte-identical resources under `src/Nwflash.Desktop/src-tauri/resources/` and the historic logo.
- Produces: React-local logo imports and a release manifest whose every `source` begins `src/Nwflash.Desktop/src-tauri/resources/`.

- [ ] **Step 1: Copy the logo into the React asset directory**

```powershell
New-Item -ItemType Directory -Force 'src/Nwflash.Desktop/src/assets' | Out-Null
Copy-Item -LiteralPath 'src/VivoKsu.App/Assets/logo.jpg' -Destination 'src/Nwflash.Desktop/src/assets/logo.jpg'
```

Expected: the new logo has the same SHA-256 as the historic source.

- [ ] **Step 2: Make both React components import the local logo**

Replace each import of `../../../VivoKsu.App/Assets/logo.jpg` with `../assets/logo.jpg` in `AppShell.tsx` and `LoginScreen.tsx`.

- [ ] **Step 3: Point all release-manifest sources at Tauri resources**

For every entry in `packaging/release/tauri-resources.json`, replace the `source` prefix `src/VivoKsu.App/` with `src/Nwflash.Desktop/src-tauri/resources/` without changing `destination` or `sha256`.

- [ ] **Step 4: Change the external-resource upload source root**

In `scripts/Upload-Resources.ps1`, set `$appSource` to `src\Nwflash.Desktop\src-tauri\resources` and update comments to describe Rust/Tauri resources.

- [ ] **Step 5: Verify resource identity and React compilation**

```powershell
$source = 'src/VivoKsu.App/Assets/logo.jpg'
$target = 'src/Nwflash.Desktop/src/assets/logo.jpg'
if ((Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash -ne (Get-FileHash -LiteralPath $target -Algorithm SHA256).Hash) { throw 'logo hash mismatch' }
npm run build --prefix src/Nwflash.Desktop
```

Expected: matching logo hashes and a successful Vite production build.

### Task 2: Remove Rust runtime dependence on C# migration inputs

**Files:**
- Modify: `src/Nwflash.Desktop/src-tauri/tests/e2e/api.rs`
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-windows/tests/driver_installer.rs`

**Interfaces:**
- Consumes: the active Tauri driver archive at `src/Nwflash.Desktop/src-tauri/resources/drivers/vivo-usb-driver.7z`.
- Produces: Rust tests that use only Rust/Tauri assets and test contracts.

- [ ] **Step 1: Remove the C# migration-map test helpers and tests**

Delete `MAPPING_PATH`, `CSHARP_TESTS_PATH`, mapping-row parsing, evidence validation helpers, and the four tests that validate C# test mapping from `api.rs`. Preserve helpers and tests used by other API contract cases.

- [ ] **Step 2: Retarget the bundled driver archive test**

Change `bundled_driver_archive_extracts_inf_files_before_pnputil_can_run` to locate `resources/drivers/vivo-usb-driver.7z` by walking from `nwflash-windows` to `src-tauri`, not through `src/VivoKsu.App`.

- [ ] **Step 3: Run the affected Rust tests**

```powershell
cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --test e2e
cargo test --manifest-path src/Nwflash.Desktop/src-tauri/crates/nwflash-windows/Cargo.toml --test driver_installer
```

Expected: both commands complete with zero failures before C# files are moved.

### Task 3: Move the C# version into its archive

**Files:**
- Move: `VivoKsu.slnx` to `archive/csharp/VivoKsu.slnx`
- Move: `src/VivoKsu.App/` to `archive/csharp/src/VivoKsu.App/`
- Move: `src/VivoKsu.Bootstrapper/` to `archive/csharp/src/VivoKsu.Bootstrapper/`
- Move: `tests/VivoKsu.App.Tests/` to `archive/csharp/tests/VivoKsu.App.Tests/`
- Move: `tests/VivoKsu.VisualCapture/` to `archive/csharp/tests/VivoKsu.VisualCapture/`
- Move: WPF-only scripts to `archive/csharp/scripts/`
- Move: WPF/migration historical docs to `archive/csharp/docs/`
- Create: `archive/csharp/README.md`

**Interfaces:**
- Consumes: the complete historical C# source tree.
- Produces: an archive whose moved `.slnx` retains valid relative references.

- [ ] **Step 1: Verify explicit source paths before moving**

```powershell
$paths = @(
  'VivoKsu.slnx', 'src/VivoKsu.App', 'src/VivoKsu.Bootstrapper',
  'tests/VivoKsu.App.Tests', 'tests/VivoKsu.VisualCapture',
  'scripts/Publish-Release.ps1', 'scripts/capture-lineflash.ps1',
  'scripts/list-flashprep-buttons.ps1', 'scripts/read-flashprep-deep.ps1',
  'scripts/read-flashprep-status.ps1', 'scripts/verify-flashprep-download.ps1',
  'scripts/verify-partition-read.ps1', 'scripts/verify-progress.ps1',
  'scripts/verify-software-page.ps1', 'docs/architecture.md',
  'docs/safeflash-ota.md', 'docs/architecture-tauri-migration.md',
  'docs/migration-baselines/tauri-test-mapping.md'
)
$paths | ForEach-Object { if (-not (Test-Path -LiteralPath $_)) { throw "missing source: $_" } }
```

Expected: every listed source exists before `git mv` is used.

- [ ] **Step 2: Create archive parents and move tracked files with Git**

```powershell
New-Item -ItemType Directory -Force 'archive/csharp/src','archive/csharp/tests','archive/csharp/scripts','archive/csharp/docs/migration-baselines' | Out-Null
git mv VivoKsu.slnx archive/csharp/VivoKsu.slnx
git mv src/VivoKsu.App archive/csharp/src/VivoKsu.App
git mv src/VivoKsu.Bootstrapper archive/csharp/src/VivoKsu.Bootstrapper
git mv tests/VivoKsu.App.Tests archive/csharp/tests/VivoKsu.App.Tests
git mv tests/VivoKsu.VisualCapture archive/csharp/tests/VivoKsu.VisualCapture
```

Then move the nine named WPF-only scripts and the four named historical documents with `git mv` into their matching `archive/csharp/` paths.

- [ ] **Step 3: Add the archive guide**

The guide must state that C# is frozen, identify the active client as `src/Nwflash.Desktop/`, and document:

```powershell
Set-Location archive/csharp
dotnet test tests/VivoKsu.App.Tests/VivoKsu.App.Tests.csproj -c Debug
```

- [ ] **Step 4: Verify the archive solution paths resolve**

```powershell
dotnet build archive/csharp/VivoKsu.slnx -c Debug
```

Expected: every archived project path in the solution is readable from its new location.

### Task 4: Make active documentation describe the Rust-only product

**Files:**
- Modify: `README.md`
- Modify: `docs/index.md`
- Modify: `docs/project-architecture.md`
- Modify: `src/Nwflash.Desktop/README.md`
- Modify: `scripts/Test-TauriRelease.ps1`

**Interfaces:**
- Consumes: the archive location and active Rust/Tauri source paths.
- Produces: documentation whose build, test, release, and development instructions target only `src/Nwflash.Desktop/`.

- [ ] **Step 1: Replace active C# source maps and commands**

Remove C# build/test/launch/publish instructions from active README and docs. Add Rust workspace commands:

```powershell
npm run test --prefix src/Nwflash.Desktop
npm run build --prefix src/Nwflash.Desktop
cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --workspace
powershell -ExecutionPolicy Bypass -File scripts/Publish-TauriRelease.ps1
```

- [ ] **Step 2: Mark C# as archival history**

Link active docs to `archive/csharp/README.md` for recovery information, not to C# source directories as active implementation targets.

- [ ] **Step 3: Remove C# migration-gate instructions from the Tauri README**

Delete the C# mapping command and describe Rust workspace tests as self-contained.

- [ ] **Step 4: Keep the release-contract test on active documentation**

In `scripts/Test-TauriRelease.ps1`, replace the retired document list
`README.md`, `docs\\architecture.md`, `docs\\architecture-tauri-migration.md`
with `README.md`, `docs\\project-architecture.md`, and
`src\\Nwflash.Desktop\\README.md`. Ensure all three active documents mention
`Verify-TauriRelease.ps1`.

### Task 5: Prove separation and commit the archive

**Files:**
- Verify: active source, scripts, packaging, and documentation
- Verify: `archive/csharp/`

**Interfaces:**
- Consumes: the completed archive and Rust-only active tree.
- Produces: evidence that current Rust development no longer depends on C#.

- [ ] **Step 1: Check active C# path references are absent**

```powershell
rg -n "src/VivoKsu\.App|src/VivoKsu\.Bootstrapper|tests/VivoKsu\.App\.Tests|VivoKsu\.slnx" `
  README.md docs src/Nwflash.Desktop scripts packaging `
  -g '!docs/superpowers/**'
```

Expected: no matches outside `archive/csharp/`; the command exits with code 1 because no active references remain.

- [ ] **Step 2: Run the complete active-client verification suite**

```powershell
cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --workspace
npm run test --prefix src/Nwflash.Desktop
npm run build --prefix src/Nwflash.Desktop
powershell -ExecutionPolicy Bypass -File scripts/Test-TauriRelease.ps1
```

Expected: every active Rust/Tauri verification command succeeds.

- [ ] **Step 3: Run archived C# tests from the archive root**

```powershell
dotnet test archive/csharp/tests/VivoKsu.App.Tests/VivoKsu.App.Tests.csproj -c Debug
```

Expected: the archive remains buildable and testable without being part of active Rust release flows.

- [ ] **Step 4: Commit the separation**

```powershell
git add --all
git commit -m "refactor: archive csharp desktop version"
```
