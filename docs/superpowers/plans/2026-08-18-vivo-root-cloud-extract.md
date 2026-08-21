# Vivo ROOT 云端 OTA 提取（boot/vendor_boot 云提取）实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 给 Vivo ROOT 页加「从服务器获取」来源：页开/手动检测时经 `/api/rom` 解析 OTA，勾选后以 HTTP Range 只按需提取修补所需启动镜像（init_boot 优先、boot 回退、vendor_boot），产出镜像灌入现有 `RootImageRuntime`，后续 KSU 修补/刷写流程原样复用。

**Architecture:** 复用现有分层。`nwflash-infrastructure` 新增 `remote_firmware`（远端格式探测 + `RangeHttpReader`（Read+Seek 走 HTTP Range）+ 直接镜像 zip 成员定向提取）；`nwflash-application` 新增 `RootOtaService` 编排（payload OTA 直接把 URL 喂给现有 `FirmwareExtractService`；直接镜像 zip 走 infra 提取）；`nwflash-tauri` 新增 `commands/root_ota.rs`（`root_ota_check`/`root_ota_extract_images` + `RootOtaRuntime` 缓存，URL 留 Rust 侧）；`commands/root.rs` 做 boot/init_boot 分区名穿通；`RootPage.tsx` 加来源单选 + 检测按钮。

**Tech Stack:** Rust（reqwest blocking + rustls、zip 4.2.0、payload_dumper 子进程）、Tauri command、React（vitest 测试）。

## Global Constraints

- 前端绝不拿到 URL / serial / pd / version / staging 路径；DTO 只含不透明 ID + 显示标签 + 大小。
- ROM 解析留在 Rust 内部；`cloudflare/**` 契约目录不得改动。
- 所有耗时操作经 `OperationCoordinator`（互斥 / 取消 / 进度 / 日志）。
- 分区名穿通端点：`init_boot`（优先）/ `boot`（回退）→ Vivo KSU 槽位；`vendor_boot` → 官版 KSU 槽位。
- 测试一律本机 mock：HTTP 用测试内 Range TCP server / wiremock，payload_dumper 用录制输出或注入执行器；不需要真实 token/设备。
- payload_dumper SHA-256 固定 `031b404609e804cd620fb10efdfce577b633f8b0ad8029fbd7170be3bc4cbe82`（`remote_assets.rs` `PAYLOAD_DUMPER_SHA256`）。
- 每个任务都要可单独测试并提交，提交信息带 `Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>`。

---

### Task 1: 共享设备 PD/版本读取 helper

**Files:**
- Create: `src-tauri/crates/nwflash-tauri/src/commands/device_identity.rs`
- Modify: `src-tauri/crates/nwflash-tauri/src/commands/mod.rs`（加 `pub mod device_identity;`）
- Modify: `src-tauri/crates/nwflash-tauri/src/commands/safe_flash.rs`（删私有 `read_online_ota_identity`/`online_ota_identity_from_process_output`/`online_ota_identity_from_getprop`，改调共享版）

**Interfaces:**
- Consumes: `nwflash_windows::device_transport::DeviceTransport`、`PlatformTools`、`process::run_command`、`ProcessOutput`。
- Produces:
  ```rust
  pub async fn read_online_ota_identity(serial: &str) -> Result<(String, String), String>;
  pub fn online_ota_identity_from_process_output(output: ProcessOutput) -> Result<(String, String), String>;
  pub fn online_ota_identity_from_getprop(output: &str) -> Result<(String, String), String>;
  ```
  语义与现实现完全一致（PD=`ro.product.device`；版本=`ro.build.version.bbk` 末段 → 回退 display.id → version.incremental → vivo.os.build.display.id）。

- [ ] **Step 1: 建新模块并把原函数整体搬入**（`device_identity.rs`，代码照抄 safe_flash.rs 原实现 + 头注释「供 Root 云提取与安全刷写共用」）。
- [ ] **Step 2: safe_flash.rs 改为 `use crate::commands::device_identity::{read_online_ota_identity, online_ota_identity_from_process_output};` 并删私有版本**。
- [ ] **Step 3: 迁移证明测试**——在 `device_identity.rs` 底部 `#[cfg(test)]` 放原 safe_flash 测试 `device_identity_failure_does_not_expose_adb_output`（不变），并新增一条：`online_ota_identity_from_getprop` 从完整 bbk 行解析出版本末段：
  输入 `[ro.build.version.bbk]: [DPD2221B_A_16.2.12.0.W10.V000L1]` + `[ro.product.device]: [PD2417]` → 期望 `("PD2417", "16.2.12.0.W10.V000L1")`。
- [ ] **Step 4: 运行 crate 测试**：
  `cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml -p nwflash-tauri device_identity -- --nocapture`
  期望：PASS。
- [ ] **Step 5: 提交**（`git add src-tauri/crates/nwflash-tauri/src/commands/`，`git commit -m "refactor(root): 共享设备 PD/版本读取 helper"`）。

---

### Task 2: infra `remote_firmware` —— RangeHttpReader + 探测 + 直接镜像 zip 成员提取

**Files:**
- Modify: `src-tauri/crates/nwflash-infrastructure/Cargo.toml`（reqwest 加 `blocking` feature）
- Create: `src-tauri/crates/nwflash-infrastructure/src/remote_firmware.rs`
- Modify: `src-tauri/crates/nwflash-infrastructure/src/lib.rs`（`pub mod remote_firmware;` 导出）
- Create: `src-tauri/crates/nwflash-infrastructure/tests/common/mod.rs`（Range mock TCP server）
- Create: `src-tauri/crates/nwflash-infrastructure/tests/remote_firmware.rs`

**Interfaces:**
- Consumes: reqwest::blocking::Client（新增 feature）、zip 4.2.0、io。
- Produces:
  ```rust
  pub enum RemoteFirmwareKind { PayloadZip, PayloadRaw, DirectImageZip, Unsupported }
  pub enum RemoteFirmwareError { InvalidUrl(String), Transport(String), RangeUnsupported,
      UnsupportedFormat, Archive(String), MissingPartition(String), Integrity(String), Cancelled }
  pub struct ZipMember { pub name: String, pub index: usize, pub size_bytes: i64 }
  pub struct ExtractedZipImage { pub partition_name: String, pub output_path: String, pub size_bytes: i64 }
  pub fn probe_remote_kind(url, client, is_canceled) -> Result<RemoteFirmwareKind, RemoteFirmwareError>;
  pub fn list_zip_members(url, client, is_canceled) -> Result<Vec<ZipMember>, RemoteFirmwareError>;
  pub fn extract_zip_members(url, client, wanted: &[&str], output_dir, is_canceled,
      report_progress: &dyn Fn(&str, u64)) -> Result<Vec<ExtractedZipImage>, RemoteFirmwareError>;
  pub struct RangeHttpReader; impl Read + Seek;
  pub fn range_http_reader(url, client, is_canceled) -> Result<RangeHttpReader, RemoteFirmwareError>;
  ```

**实现要点：**
- `RangeHttpReader`：字段 `client, url, total_len, pos, fill: Vec<u8>, fill_pos, is_canceled`。`new` 发 `Range: bytes=0-0`，要求 206 + 解析 Content-Range 总长；非 206 → `RangeUnsupported`。`Read` 从 fill 缓冲读取，不足时以 `CHUNK=1MB` 为上限 Range 拉 `[pos, min(pos+CHUNK, total_len)-1]`（每次拉取前查 `is_canceled` → `io::ErrorKind::Interrupted`）。`Seek` 清空 fill、设 `pos`；支持 `Start/End/Current`。`total_len()` 供内部使用。
- `probe_remote_kind`：首 4 字节 Range：`PK 03 04`→zip；`CrAU`→PayloadRaw；`1f 8b`→Unsupported；其它→Unsupported。zip 分支：`list_zip_members` 查是否含文件名基名=payload.bin → PayloadZip / DirectImageZip。
- `extract_zip_members`：`ZipArchive::new(RangeHttpReader)`；遍历 `file_names()` 选基名命中 wanted 且 `.img`/`.bin` 的成员（去掉目录项，基名非空、不以 payload/bootloader 特例限制）；对每个命中成员 `by_name(full_name)`，读至 EOF（手动 buffer 循环以注入 `report_progress(分区基名, 已写字节)`）写 `output_dir/{基名}`；校验：写入字节数与 CD uncompressed size 一致，CRC 用 member 的 `crc32()`（如 v4 暴露）或读毕一致；失败 → `Integrity`/`MissingPartition`。中途取消 → `Cancelled`。
- 成员去重：同基名取第一个。
- `ExtractedZipImage.partition_name` = 去扩展的基名（如 `boot`、`vendor_boot`、`init_boot`）。

- [ ] **Step 1: Cargo.toml 给 reqwest 加 `blocking`**。
- [ ] **Step 2: 写 Range mock server**（`tests/common/mod.rs`）：
  ```rust
  // 启动 127.0.0.1:0 TcpListener，逐请求解析:首行 GET / HTTP/1.1 + Range: bytes=a-b
  // 响应: 无 Range → 200 + 全长; a/b 有效 → 206 + Content-Range: bytes a-b/total + 切片;
  // 越界 → 416。返回 server 地址。
  pub fn spawn_range_server(data: Vec<u8>) -> String; // "http://127.0.0.1:PORT/"
  ```
  （实现要点：每次 accept 起一个线程，读请求头到空行，解析 Range，按上面规则写响应，close。会话保持 keep-alive 可忽略——Req 客户端会重连。）
- [ ] **Step 3: 写失败测试**（`tests/remote_firmware.rs`）：
  - `probe_kind_recognizes_each_magic`：PK/CrAU/1f8b/其它 四态。
  - `range_reader_reports_length_and_reads_spans`：用 5MB 随机数据，读 [0..8)、seek(End) 读 eocd、seek 任意偏移读 1MB 块，断言内容一致。
  - `probe_zip_distinguishes_payload_zip_from_direct_image_zip`：一个含 `payload.bin` 的 zip fixture vs 一个含 `boot.img` 的 zip fixture（用 `zip4::{write::SimpleFileOptions, ZipWriter}` 构造本地字节）。
  - `extract_direct_zip_members_fetches_only_wanted_members`：构造含 `boot.img`、`vendor_boot.img`、`system.new.dat.0` 的 zip → 请求 `["init_boot","boot","vendor_boot"]` → 得到 boot+vendor_boot，system 未提取。
  - `extract_zip_missing_partition_errors`：zip 无 boot/vendor_boot → `MissingPartition`。
  - `range_reader_rejects_non_range_server`：mock server 对 Range 返回 200 全长 → `RangeUnsupported`。
  - `zip64_offset_zip_is_supported`：手写一个最小 ZIP64 fixture（2 个成员，EOCD64 定位器 + ZIP64 extra），断言能列出并提取（见下方 helper 骨架）。
  - `extraction_reports_progress_bytes`：提取时断言累计字节单调且等于成员大小。
  - `cancel_aborts_extraction`：is_canceled 恒真 → `Cancelled`。
- [ ] **Step 4: 实现 `remote_firmware.rs`** 至测试通过（含 ZIP64 fixture helper：按 zip 规范手拼 local header/CD/EOCD64；两个成员 `boot.img`（deflate 或 stored）、`vendor_boot.img`（stored））。
- [ ] **Step 5: 运行测试**：`cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml -p nwflash-infrastructure --test remote_firmware`
  期望：全 PASS。
- [ ] **Step 6: 提交**。

---

### Task 3: application `RootOtaService` 编排

**Files:**
- Create: `src-tauri/crates/nwflash-application/src/root_ota.rs`
- Modify: `src-tauri/crates/nwflash-application/src/lib.rs`（导出 `RootOtaService` 等）
- Create: `src-tauri/crates/nwflash-application/tests/root_ota.rs`

**Interfaces:**
- Consumes: `crate::FirmwareExtractService`、`FirmwareExtractEntry`、`nwflash_infrastructure::{probe_remote_kind, extract_zip_members, remote_firmware::RemoteFirmwareKind}`、`nwflash_domain::{DomainError, FlashImageInfo}`、`std::path::{Path, PathBuf}`、`tokio_util::sync::CancellationToken`。
- Produces:
  ```rust
  pub struct RootOtaExtractOptions<'a> { pub url: &'a str, pub payload_dumper: Option<&'a Path>, pub staging_root: &'a Path }
  pub struct RootOtaExtractedImages {
      pub boot_image: Option<FlashImageInfo>, pub boot_partition_name: String,
      pub vendor_boot: Option<FlashImageInfo>, pub staging_root: PathBuf,
  }
  pub struct RootOtaService;
  impl RootOtaService {
      pub fn new() -> Self;
      pub fn extract<F, S, P>(&self, options: RootOtaExtractOptions<'_>,
          is_canceled: F, report_stage: S, report_progress: P)
          -> Result<RootOtaExtractedImages, DomainError>
      where F: FnMut() -> bool + Send + 'static, S: FnMut(String) + Send + 'static,
            P: FnMut(f64) + Send + 'static;
  }
  ```

**实现要点：**
- `extract` 内顺序：probe → 分支。
- **PayloadZip / PayloadRaw**：`payload_dumper` 必提供（缺 → `DomainError::ExternalTool("payload 提取工具未就绪。")`）。`inspect_payload(exe, url, meta_dir, cancel)` 列分区 → 过滤出 boot 槽位（`init_boot` 存在用 init_boot，否则 `boot`）+ `vendor_boot`（存在用之）。`extract_payload_with_expected_sizes_and_progress(exe, url, selected, staging, cancel, |part, bytes| 报进度)`。产出 `FlashImageInfo{path, size}`。
- **DirectImageZip**：`extract_zip_members(url, &blocking_client, &["init_boot","boot","vendor_boot"], staging, cancel, |name, bytes| 报进度)` → 同选址规则映射 `boot_image`/`vendor_boot`。
- **Unsupported** → `DomainError::InvalidFormat("不支持的 OTA 格式，无法云提取 ROOT 分区。")`。
- 无任何 boot 镜像 → `DomainError::InvalidOperation("该 OTA 不含可修补的 boot/init_boot 分区。")`。
- `boot_partition_name`：选中来源的实际名（`init_boot` 或 `boot`）；无 boot 镜像时留空。
- staging 目录由 `RootOtaService` 创建（`create_staging_root` 风格，同 `safe_flash.rs` 唯一序列号 + pid + 时间戳，根 `std::env::temp_dir().join("nwflash-root-ota")`），失败清理。
- 错误映射：`RemoteFirmwareError` → `DomainError`（Cancelled→UserCancelled、RangeUnsupported/Transport/UnsupportedFormat→InvalidOperation、Archive/Integrity→InvalidFormat、MissingPartition→InvalidOperation）。

- [ ] **Step 1: 写失败测试**（`tests/root_ota.rs`，payload 路径用「真实 payload_dumper 不可用→依赖注入不可行」则改为**录制断言**：不直接调用，而是测试选址纯函数 + 通过本地 Range server 喂一个真实小 payload 时要求 payload_dumper 存在并返回工具未就绪错误）：
  - `extract_requires_payload_dumper_for_payload_kind`：URL 指向含 payload.bin 的 zip fixture（本地 Range server），payload_dumper=None → `ExternalTool`。
  - `zip_kind_extracts_boot_preferring_init_boot`：直接镜像 zip（含 init_boot.img+boot.img+vendor_boot.img）→ `boot_partition_name=="init_boot"`，两份镜像都产出（经真实 Range server + 无 payload_dumper 参与）。
  - `zip_kind_falls_back_to_boot`：zip 只含 boot.img+vendor_boot.img → `boot_partition_name=="boot"`。
  - `unsupported_kind_errors`：gzip 头 fixture → `InvalidFormat`。
  - `missing_boot_partition_errors`：zip 只有 system.img → `InvalidOperation`。
- [ ] **Step 2: 实现 `root_ota.rs`** 至此通过。
- [ ] **Step 3: 运行**：`cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml -p nwflash-application --test root_ota`
- [ ] **Step 4: 提交**。

---

### Task 4: `commands/root.rs` 分区名穿通（boot/init_boot）

**Files:**
- Modify: `src-tauri/crates/nwflash-tauri/src/commands/root.rs`
- Test: 同文件 `#[cfg(test)]`。

**变更：**
- `RootImageSelection`（runtime）加 `target_partition_name: String`。
- `RootImageRuntime::replace(kind, image, target_partition_name)`；`root_select_image` 传 `kind.label()`（init_boot/vendor_boot）。`get` 返回带目标名的选择。
- `RootAutomaticSelection` 加 `boot_partition_name: String`；`take_automatic` 从 init_boot 选择里带出。
- `RootPatchedArtifact` 构造：`replace_owned`/`replace` 增加显式 `flash_partition: QuickFlashPartition`（原 `kind.quick_flash_partition()` 变成由调用方按真实分区名派生：`"boot"→QuickFlashPartition::Boot`、`"init_boot"→InitBoot`、`"vendor_boot"→VendorBoot`）。
- `build_vivo_ksu_patch_commands(serial, library_path, source_path, staged_output_path, kmi, partition)`：`--partition {partition}`；远端文件名 `vivoksu_{partition}.img` / `vivoksu_patched_{partition}.img`；清理命令同参数化。`VIVO_KSU_REMOTE_SOURCE/PATCHED` 常量改函数或 format!。
- `patch_vivo_ksu_core(... , partition: &str)`：透传；产物用 `flash_partition` 派生（boot→Boot、init_boot→InitBoot）。
- `root_patch_vivo_ksu`：`runtime.get(InitBoot, id)` 现在同时返回 target_partition_name → 传入。
- `root_run_automatic`：从 `take_automatic` 带出 `boot_partition_name` → 传 `patch_vivo_ksu_core`。
- `automatic_root_flash_source`：`partition_name: artifact.partition.partition_name()`（自动跟随 Boot/InitBoot）。
- `root_prepare_patched_artifact_flash`：不变逻辑（用 `artifact.partition`）。
- 更新全部受影响测试断言（现断言 `--partition init_boot` 与 `vivoksu_init_boot.img` 的测试改为按参数化后的调用传 `"init_boot"` 仍过；新增一条 `boot` 分区的断言）。

- [ ] **Step 1: 改 runtime/结构体/函数签名与实现**（先让现有测试失败点暴露，再逐个更新断言）。
- [ ] **Step 2: 新增 boot 参数化测试**：`build_vivo_ksu_patch_commands(..., "boot")` → args 含 `--partition boot`、`vivoksu_boot.img`、`vivoksu_patched_boot.img`；`"init_boot"` 保持原断言。
- [ ] **Step 3: 运行**：`cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml -p nwflash-tauri`
- [ ] **Step 4: 提交**。

---

### Task 5: `commands/root_ota.rs` + `RootOtaRuntime` + 命令 + AppState 接线

**Files:**
- Create: `src-tauri/crates/nwflash-tauri/src/commands/root_ota.rs`
- Modify: `src-tauri/crates/nwflash-tauri/src/commands/mod.rs`（`pub mod root_ota;`）
- Modify: `src-tauri/crates/nwflash-tauri/src/lib.rs`（AppState 加 `root_ota_runtime`；构造；`generate_handler![...]` 注册 `root_ota_check`、`root_ota_extract_images`；session_stop 清理）
- Modify: `src-tauri/crates/nwflash-tauri/src/commands/root.rs`（暴露 `RootImageKind::label`、`runtime.replace` 签名已支持）

**Interfaces:**
```rust
pub struct ResolvedRootOta { pub url: String, pub name: Option<String>, pub size_bytes: Option<i64>,
                             pub pd: String, pub version: String }
pub struct RootOtaRuntime { state: Arc<Mutex<RootOtaRuntimeState>> }  // resolved + staging_root(所有者)
impl RootOtaRuntime { pub fn new(); pub fn store(&self, ResolvedRootOta) ; pub fn resolve(&self) -> Result<ResolvedRootOta,String>;
                      pub fn take_staging(&self)->Option<PathBuf>; pub fn cleanup(&self); }


#[tauri::command] pub async fn root_ota_check(state) -> Result<RootOtaCheckDto, String>;
#[tauri::command] pub async fn root_ota_extract_images(state) -> Result<RootOtaExtractResultDto, String>;

pub struct RootOtaCheckDto { pub available: bool, pub label: Option<String> }
pub struct RootOtaExtractResultDto { pub source_label: String,
    pub initBoot: Option<RootImageSelectionDto>, pub vendorBoot: Option<RootImageSelectionDto> }
```
- `session_token()`（复用 safe_flash 的私有函数，抽为 commands 内共享或重写）。
- `root_ota_check`：`device_runtime.active_adb_serial()` 失败 / token 缺失 → `Ok{available:false}`（静默）。否则 `read_online_ota_identity` → `client.resolve_rom(&token,&pd,&version)` → OK 缓存到 `RootOtaRuntime`（不含 serial）→ `{available:true,label}`；Err → `{available:false}`。
- `root_ota_extract_images`：`root_ota_runtime.resolve()` 取得受控来源，不比较任何缓存 serial；`OperationCoordinator.run_async(OperationKind::Extracting? 或 Hashing, "提取服务器 OTA 分区", ...)`：provision payload_dumper（`PayloadDumperProvisioner`，仅 payload 需要——可先 probe 后再 provision，或统一 provision 无妨：下载过一次后缓存）；`RootOtaService::extract` 到新 staging（`create_root_ota_staging`）；把结果写入 `RootImageRuntime`（`replace(InitBoot, boot_image, boot_partition_name)`、`replace(VendorBoot, vendor_boot, "vendor_boot")`）；staging 所有权移交 `RootOtaRuntime.take_staging` 前的旧 staging 清理；返回 DTO（来源 label = `ResolvedRootOta.name` 或 "服务器 OTA"）。
- `OperationKind` 需要在 domain 枚举里加 `Extracting`？—— 查看现有枚举后决定：若已有合适变体（`Hashing`/`Installing`）直接复用，否则在 `nwflash-domain::operation` 加 `Extracting` 并同步 usage 枚举/映射。
- 安全：DTO 无 URL/serial/path；输入空结构体（`deny_unknown_fields` 无意义可省）。

- [ ] **Step 1: 写 DTO + runtime 单测**（同文件 `#[cfg(test)]`）：缓存存储/读取且不保存 serial；重复检测替换缓存并清理旧 staging（构造临时目录）。
- [ ] **Step 2: 实现 runtime + 两个 command**。
- [ ] **Step 3: AppState/接线 + session_stop 清理调用**（仿现有 runtime 清理的时机）。
- [ ] **Step 4: command 层安全测试**：
  - `root_ota_extract_images` 返回 DTO 序列化不含 url/serial/pd/path；`root_ota_check` 失败时 available=false 且不抛。
- [ ] **Step 5: 运行**：`cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --workspace`
  期望：全绿（含既有 e2e）。
- [ ] **Step 6: 提交**。

---

### Task 6: 前端 RootPage.tsx UX + vitest

**Files:**
- Modify: `src/pages/RootPage.tsx`
- Modify: `src/pages/RootPage.test.tsx`

**变更（UX）：**
- state：`otaAvailable:boolean`、`otaLabel:string`、`sourceMode:'local'|'server'`。
- `refresh()` 内：`sessionState.has_token` 时追加 `root_ota_check` 调用（`invoke<RootOtaCheck>`）→ 填充 `otaAvailable/otaLabel`；失败静默。
- 「OTA 来源」UI 行（放在 manager/kmi 区块前）：radio「本地镜像」/「从服务器获取」（server 单选仅当 `otaAvailable` 可选，否则禁用提示「检测服务器 OTA」）；「检测服务器 OTA」按钮 → 再跑 `root_ota_check`。
- 勾选「从服务器获取」：清空本地选择（`setInitBoot(null);setVendorBoot(null);..`），调 `root_ota_extract_images` → `{sourceLabel, initBoot, vendorBoot}` → `setInitBoot/setVendorBoot`、`setSourceLabel`、重置 readiness/artifact。busy 锁定。
- sourceMode==='server' 时隐藏/禁用「选择 init_boot 镜像」「选择 vendor_boot」按钮（保留预检/修补/全自动等原逻辑）。
- 显示 `sourceLabel` 摘要（如「已从 {name} OTA 提取」）。
- DTO 类型定义：`RootOtaCheck {available:boolean; label?:string|null}`、`RootOtaExtractResult {sourceLabel:string; initBoot:RootImageSelection|null; vendorBoot:RootImageSelection|null}`。

**测试（vitest 模式同现文件）：**
- 开启时 `has_token=true` → 调用 `root_ota_check` 且渲染「从服务器获取」radio 可用；`available:false` → radio 禁用。
- 无 token → 不调用 `root_ota_check`。
- 检测按钮点击 → 再触发 `root_ota_check`。
- 勾选 server radio → 调用 `root_ota_extract_images`，返回结果填充 initBoot/vendorBoot 显示，路径安全（不含 `C:\`）。
- 切回 local → 恢复原选择按钮可用。

- [ ] **Step 1: 写失败测试**（`RootPage.test.tsx` 扩展）。
- [ ] **Step 2: 实现 RootPage.tsx**。
- [ ] **Step 3: 运行**：`npm run test --prefix src/Nwflash.Desktop`
  期望：全 PASS。
- [ ] **Step 4: 提交**。

---

### Task 7: 全量验证 + e2e 冒烟

- [ ] **Step 1:** `cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --workspace` 全绿。
- [ ] **Step 2:** `npm run test --prefix src/Nwflash.Desktop` 全绿。
- [ ] **Step 3:** `npm run build --prefix src/Nwflash.Desktop` 成功。
- [ ] **Step 4:** `npm run test --prefix src/Nwflash.Desktop/e2e-tests`（WDIO 冒烟；若 CI 缺设备则只跑无设备规格）。
- [ ] **Step 5:** 产物无测试 bridge（复跑 `npm run build` 校验 `VITE_NWFLASH_WDIO_E2E` 未污染）。
- [ ] **Step 6:** 提交（如有测试基础设施变更）。

---

## 自审清单（写完计划后执行）

1. **Spec 覆盖**：`root_ota_check`（Task5）、`root_ota_extract_images`（Task5）、payload 分支（Task3）、zip 分支（Task2/3）、boot 回退 + 分区穿通（Task4）、前端 radio（Task6）、测试/安全（各任务）、错误映射（Task3）。
2. **占位符扫描**：无 TODO/TBD。
3. **类型一致性**：`RootOtaExtractedImages.boot_partition_name`（Task3 产出）→ `RootImageRuntime::replace` 的 target（Task4/5 消费）命名一致；`RootImageSelectionDto` 字段 `id/kind/fileName/sizeBytes` 沿用现有；cmd `root_ota_extract_images` 返回 `initBoot/vendorBoot` camelCase 与前端类型一致。
4. **依赖顺序**：Task2（infra）→ Task3（app）→ Task4（root.rs 穿通）→ Task5（cmd）→ Task6（前端）。
