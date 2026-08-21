# Tauri Client Source Archive Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deliver a source-only ZIP of the 奶蛙Flash Tauri/Rust client together with a source-grounded Rust/Tauri architecture document, a successful client compilation, and no leftover archive staging directory.

**Architecture:** The canonical client architecture document lives next to the Tauri workspace so it is included in the source archive. The archive is assembled from a uniquely named temporary staging directory and contains one `Nwflash.Desktop/` root; generated dependencies and build/release output are excluded by exact directory name. The final production no-bundle Tauri build supplies compilation evidence, after which only the uniquely created staging directory is deleted.

**Tech Stack:** Rust 2021/Cargo, Tauri 2, React 19, Vite, PowerShell/.NET ZIP APIs.

## Global Constraints

- Do not modify `cloudflare/**`.
- Preserve existing user changes in the dirty worktree.
- Do not add serial binding in any feature; document the actual one-current-device-per-launch runtime model instead. Serial may be derived transiently by Rust while constructing the current device command, but is never cached or compared across steps.
- Do not claim Task 4 concurrent pipe draining or Task 5 ROOT image byte fingerprinting is implemented.
- Archive only client source from `src/Nwflash.Desktop/`; exclude `node_modules`, `dist`, `src-tauri/target`, `logs`, `tauri-release`, and `vmprotect`.
- Create the archive at `artifacts/source/Nwflash.Desktop-source-20260818.zip` and keep it after cleanup.
- The only directory deleted by this plan is the uniquely named `%TEMP%\\nwflash-client-source-archive-*` staging directory created for this archive.

---

### Task 1: Write the Canonical Rust/Tauri Client Architecture Document

**Files:**
- Create: `src/Nwflash.Desktop/docs/rust-tauri-architecture.md`
- Modify: `src/Nwflash.Desktop/README.md`
- Modify: `docs/index.md`

**Interfaces:**
- Consumes: the actual crate layout in `src/Nwflash.Desktop/src-tauri/Cargo.toml`, Tauri handler registration in `src-tauri/crates/nwflash-tauri/src/lib.rs`, and the established migration architecture snapshot.
- Produces: a standalone, source-included document explaining crate ownership, React-to-Tauri IPC, session-token ownership, one-current-device behavior, resource verification, operation coordination, build commands, and unfinished Task 4/5 boundaries.

- [x] **Step 1: Create the architecture document with required sections**

The document must contain these headings:

```markdown
# 奶蛙Flash Rust/Tauri 客户端架构
## 范围与事实来源
## Workspace 与依赖方向
## 运行时与 IPC 边界
## 设备与操作模型
## 资源供应与完整性
## 构建、测试与发布
## 当前未完成边界
```

- [x] **Step 2: Link the document from client and repository navigation**

Add a `## 架构文档` section to `src/Nwflash.Desktop/README.md` linking `docs/rust-tauri-architecture.md`. Add the same document to `docs/index.md` as the client-local Rust/Tauri architecture reference.

- [x] **Step 3: Verify the document is complete and has no obsolete public capabilities**

Run:

```powershell
rg -n '^## ' src/Nwflash.Desktop/docs/rust-tauri-architecture.md
rg -n 'firmware_inspect_remote_payload|quick_flash_execute_commands|concurrent pipe draining|ROOT image byte fingerprint' src/Nwflash.Desktop/docs/rust-tauri-architecture.md
```

Expected: all eight headings are present; only the two unfinished features appear as explicit negative statements; no removed public command is described as available.

### Task 2: Assemble and Verify the Source-only ZIP

**Files:**
- Create: `artifacts/source/Nwflash.Desktop-source-20260818.zip`
- Create: `artifacts/source/Nwflash.Desktop-source-20260818.zip.sha256`
- Temporary: `%TEMP%\\nwflash-client-source-archive-<GUID>\\Nwflash.Desktop`

**Interfaces:**
- Consumes: final `src/Nwflash.Desktop/` client tree after Task 1.
- Produces: a ZIP whose root folder is `Nwflash.Desktop/`, includes source/configuration/resources/tests/docs, and contains no generated dependency/build/release directories.

- [x] **Step 1: Build a staging tree with explicit excluded directory names**

Use a PowerShell file enumeration that skips files whose relative path has any of these directory components:

```powershell
$excludedDirectoryNames = @('node_modules', 'dist', 'target', 'logs', 'tauri-release', 'vmprotect', 'gen')
```

Copy every other file under `src/Nwflash.Desktop/` to `%TEMP%\\nwflash-client-source-archive-<GUID>\\Nwflash.Desktop`, preserving its relative path.

- [x] **Step 2: Create the archive and SHA-256 sidecar**

Use `Compress-Archive` with the staged `Nwflash.Desktop` directory as its only input and write the archive to `artifacts/source/Nwflash.Desktop-source-20260818.zip`. Write the uppercase SHA-256 and archive filename to `artifacts/source/Nwflash.Desktop-source-20260818.zip.sha256` in ASCII.

- [x] **Step 3: Verify archive content before compilation**

Run a .NET `System.IO.Compression.ZipFile` entry scan and assert:

```powershell
$requiredEntries = @(
  'Nwflash.Desktop/README.md',
  'Nwflash.Desktop/docs/rust-tauri-architecture.md',
  'Nwflash.Desktop/src-tauri/Cargo.toml',
  'Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/lib.rs',
  'Nwflash.Desktop/src/app/App.tsx'
)
$forbiddenSegment = '/(node_modules|dist|target|logs|tauri-release|vmprotect|gen)/'
```

Expected: every required entry exists and no entry matches `$forbiddenSegment`.

### Task 3: Compile Once, Remove Archive Staging, and Verify Delivery Files

**Files:**
- Verify: `src/Nwflash.Desktop/dist/`
- Verify: `src/Nwflash.Desktop/src-tauri/target/release/nwflash-desktop.exe`
- Delete: only the exact temporary directory created by Task 2

**Interfaces:**
- Consumes: archive verified in Task 2 and current client source.
- Produces: fresh production no-bundle compilation evidence while leaving the archive and its SHA-256 sidecar intact.

- [x] **Step 1: Compile the Tauri client once**

Run:

```powershell
npm --prefix src/Nwflash.Desktop run tauri -- build --no-bundle
```

Expected: exit code `0`; Vite builds the frontend and Cargo builds the `nwflash-desktop` release executable.

- [x] **Step 2: Delete only the created staging directory**

Before deletion, resolve the exact staging path and verify it starts with `%TEMP%\\nwflash-client-source-archive-`. Then run:

```powershell
Remove-Item -LiteralPath $stageRoot -Recurse -Force
```

- [x] **Step 3: Verify the archive survives cleanup and report it**

Run:

```powershell
Test-Path artifacts/source/Nwflash.Desktop-source-20260818.zip
Test-Path artifacts/source/Nwflash.Desktop-source-20260818.zip.sha256
Test-Path $stageRoot
Get-FileHash artifacts/source/Nwflash.Desktop-source-20260818.zip -Algorithm SHA256
```

Expected: ZIP and sidecar are `True`, staging directory is `False`, and the displayed hash equals the sidecar hash.

## Completion Evidence (2026-08-18)

- Source archive: `artifacts/source/Nwflash.Desktop-source-20260818.zip`, 219 entries, 17,082,063 bytes.
- SHA-256: `142E21C3DD4BD11F5B27829A2BCA9047E1D8AEAEE43BF0814ED090CA73155A65`.
- Build: `npm --prefix src/Nwflash.Desktop run tauri -- build --no-bundle`, exit code 0; `src-tauri/target/release/nwflash-desktop.exe` exists.
- Cleanup: the uniquely created `%TEMP%\\nwflash-client-source-archive-*` directory was removed; a final temp scan found zero matching staging directories.
- Existing `dist/` and `src-tauri/target/` caches were pre-existing and were preserved.
