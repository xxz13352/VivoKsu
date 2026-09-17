# Rust Wipe Data Image Replacement Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace only the Rust/Tauri VIVO line-flash embedded wipe-data image with the verified reverse-engineered `misc` image.

**Architecture:** Keep the existing `include_bytes!` resource path and Rust flash orchestration unchanged. Replace the bytes at the infrastructure crate's existing `assets/wipe-data.img` path, so `write_wipe_data_image` and the later `misc` flash operation automatically use the new image.

**Tech Stack:** Rust/Tauri workspace, PowerShell file checks, SHA-256 verification, Cargo tests/build checks.

## Global Constraints

- Modify only `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/assets/wipe-data.img` for the implementation change.
- Do not modify `src/VivoKsu.App/Assets/wipe-data.img` or any WPF implementation file.
- Preserve the resource filename, `include_bytes!` path, `misc` partition target, and existing flash timing.
- Source bytes must come from `C:\Users\17254\Desktop\TOOL\vivo 服务逆向\misc_bcb_native_wipe_data_all.img`.
- The replacement is valid only when source and target size and SHA-256 match, and the target hash differs from the pre-change hash.

---

### Task 1: Capture source and target invariants

**Files:**
- Read: `C:\Users\17254\Desktop\TOOL\vivo 服务逆向\misc_bcb_native_wipe_data_all.img`
- Read: `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/assets/wipe-data.img`

**Interfaces:**
- Consumes: the source image and current Rust asset.
- Produces: recorded size and SHA-256 values for replacement verification.

- [ ] **Step 1: Confirm both files exist and are regular files**

Run from the repository root:

```powershell
$source = 'C:\Users\17254\Desktop\TOOL\vivo 服务逆向\misc_bcb_native_wipe_data_all.img'
$target = 'src\Nwflash.Desktop\src-tauri\crates\nwflash-infrastructure\assets\wipe-data.img'
Get-Item -LiteralPath $source,$target | Select-Object FullName,Length
```

Expected: both paths exist and each is `524288` bytes.

- [ ] **Step 2: Record the pre-change target hash**

```powershell
Get-FileHash -LiteralPath $target -Algorithm SHA256
```

Expected pre-change target hash: `D969F7165168F9836056C982378F6F94C75589654B28DC3F75391F004CDBA489`.

### Task 2: Replace the Rust embedded resource

**Files:**
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/assets/wipe-data.img`

**Interfaces:**
- Consumes: `misc_bcb_native_wipe_data_all.img`.
- Produces: the same resource filename containing the new image bytes for `include_bytes!`.

- [ ] **Step 1: Copy the source image over the Rust asset**

Run from the repository root:

```powershell
Copy-Item -LiteralPath 'C:\Users\17254\Desktop\TOOL\vivo 服务逆向\misc_bcb_native_wipe_data_all.img' -Destination 'src\Nwflash.Desktop\src-tauri\crates\nwflash-infrastructure\assets\wipe-data.img' -Force
```

Expected: only the Rust asset content changes; the target remains at the existing resource path.

### Task 3: Verify resource identity and Rust integration

**Files:**
- Read: `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/src/embedded_assets.rs`
- Read: `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/src/safe_flash.rs`
- Test: `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure`

**Interfaces:**
- Consumes: the replaced Rust asset.
- Produces: evidence that the Rust crate still embeds the new bytes and the `misc` flash flow is unchanged.

- [ ] **Step 1: Verify source and target now match**

```powershell
$source = 'C:\Users\17254\Desktop\TOOL\vivo 服务逆向\misc_bcb_native_wipe_data_all.img'
$target = 'src\Nwflash.Desktop\src-tauri\crates\nwflash-infrastructure\assets\wipe-data.img'
foreach ($path in @($source,$target)) {
    $item = Get-Item -LiteralPath $path
    $hash = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash
    "{0}|{1}|{2}" -f $item.FullName,$item.Length,$hash
}
```

Expected: both lines report `524288` and the same SHA-256; the hash is different from the pre-change target hash.

- [ ] **Step 2: Verify the Rust include path and misc target remain unchanged**

```powershell
rg -n "include_bytes!.*assets/wipe-data\.img|WIPE_DATA_PARTITION|write_wipe_data_image" `
  src\Nwflash.Desktop\src-tauri\crates\nwflash-infrastructure\src\embedded_assets.rs `
  src\Nwflash.Desktop\src-tauri\crates\nwflash-application\src\safe_flash.rs
```

Expected: `embedded_assets.rs` still includes `assets/wipe-data.img`, the application still targets `misc`, and it still calls `write_wipe_data_image`.

- [ ] **Step 3: Run the focused Rust crate tests**

```powershell
cargo test -p nwflash-infrastructure
```

Expected: the command completes successfully. If the workspace does not expose that package name, use the package name from `src\Nwflash.Desktop\src-tauri\crates\nwflash-infrastructure\Cargo.toml` and report the exact result.

- [ ] **Step 4: Confirm no WPF resource or unrelated file changed**

```powershell
git status --short
git diff --stat
git diff -- 'src/VivoKsu.App/Assets/wipe-data.img'
```

Expected: the WPF asset has no diff; the only implementation diff is the Rust binary asset, plus the already committed design and plan documentation.

- [ ] **Step 5: Commit the implementation**

```powershell
git add -- 'src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/assets/wipe-data.img'
git commit -m "fix: update rust wipe data image"
```
