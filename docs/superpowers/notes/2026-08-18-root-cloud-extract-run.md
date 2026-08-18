# 2026-08-18 自主运行落地笔记：ROOT 云端 OTA 提取 + 全局审查

> 本文件是本次自主运行的**精华落地文档**，防止上下文丢失。每次阶段完成更新。
> 目标（用户 `/goal`）：实现 ROOT 云提取 → 测试 → 审 BUG → 架构文档 → 清理临时文件 → Rust 漏洞审查修复 → taste-skill UI 优化 → 新功能提案（只写文档不落实）。中途不询问。

## 运行状态看板

- [x] 探索 + 设计（brainstorming 完成，spec 已提交 `07e3f4d`）
- [x] 实现计划（writing-plans）
- [x] 功能实现（root_ota_check / root_ota_extract_images / RangeHttpReader / 分区名穿通 / 前端 UX）
- [x] 全量测试绿（2026-08-18，再次验证后待提交）
- [x] 功能 BUG 审查 + 修复（见本节最新复查）
- [x] 项目架构文档更新
- [x] 清理临时文件
- [ ] Rust 代码漏洞/安全审查 + 修复 + 再审
- [ ] taste-skill UI 优化 + 测试
- [ ] 新功能提案文档（docs/superpowers/plans/ 或 docs/proposals/）

## 关键事实（不可丢失）

### 云提取实测结论（2026-08-18，用户给两个真实 OTA 链接）

| 链接 | 格式 | 提取 | 实测 |
|---|---|---|---|
| OTA1 9.3GB PD2417 | payload OTA（zip 含 payload.bin） | `payload_dumper.exe <URL> -i init_boot,vendor_boot` | 9.8s 成功，init_boot.img 8MB + vendor_boot.img 128MB |
| OTA2 9.4GB PD2183 | 直接镜像 zip + 块式（无 payload.bin） | zip 中央目录 Range 解析 + 成员定向抓取（ZIP64+CRC） | ~1s 取 boot.img 96MB + vendor_boot.img 64MB |

- **payload_dumper 的 PAYLOAD 参数直接接受 URL**（本地 .bin/.zip/远程 URL），远程时只按需 Range 读取不整包下载。`-i` 过滤分区，`-m/--metadata` 列分区。捆绑 exe 路径：`src/VivoKsu.App/payload-tools/payload_dumper.exe`，SHA256 固定 `031b40...cbe82`（`remote_assets.rs`）。
- OTA1 payload 内同时有 `init_boot` + `boot`；OTA2 只有 `boot` + `vendor_boot`（**无 init_boot**）→ 分区名穿通（用户已确认：boot 回退 + 分区名穿通）。
- 两个 OTA 都支持 HTTP Range（206）；服务器要 User-Agent 才返回有效内容。
- 探测脚本：`/tmp/probe_zip.py`（ZIP64 中央目录解析）、`/tmp/zip_member.py`（Range 成员提取验证）——待清理。

### 架构/边界（改动必须遵守）

- `cloudflare/**` 是独立后端契约目录，桌面迁移测试不得修改。
- 前端不接触 URL / serial / pd / version / staging 路径。DTO 只含不透明 ID + 显示标签 + 大小。
- ROM 解析留在 Rust 内部，由受控设备信息 + 内存 token 驱动（架构文档 `docs/rust-tauri-architecture.md`）。
- `OperationCoordinator` 串行化所有页面操作；取消/进度/日志统一。
- 单一设备模型；操作开始从 DeviceRuntime 当前快照派生 ADB/Fastboot serial。

### 现有代码关键位置

- `commands/safe_flash.rs`：`read_online_ota_identity`（读设备 PD/版本，私有，要共享）、`safe_flash_prepare_online`（resolve_rom→provision→resolve_source(Online) 全量下载流程）、`sanitize_safe_flash_error`。
- `commands/root.rs`：`RootImageRuntime`（不透明 ID）、`RootPatchedArtifactRuntime`（owned staging）、`build_vivo_ksu_patch_commands`（硬编码 `--partition init_boot`）、`automatic_root_flash_source`、`build_preset_execution_plan` 在 `commands/quick_flash.rs`。
- `nwflash-application/src/firmware_extract.rs`：`FirmwareExtractService::inspect_payload`、`extract_payload_with_expected_sizes_and_progress`（URL 直传即可复用）。
- `nwflash-infrastructure`：`zip` crate **4.2.0**（`zip::ZipArchive` API）；`reqwest 0.12.8` default-features=false + rustls；`payload_provisioner.rs`（PaylDumperProvisioner）；`remote_assets.rs`（SHA/镜像）。
- `FirmwareExtractPage`（React）已有 `firmware:progress` 事件 + `OperationProgressPanel`。
- 测试：Rust e2e 在 `src-tauri/tests/e2e/*.rs`；crate 单测在 `crates/*/tests/` 与 `#[cfg(test)]`；React vitest `src/pages/*.test.tsx`；WDIO e2e `e2e-tests/`。

### 命令

```powershell
cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --workspace   # Rust 全量
npm run test --prefix src/Nwflash.Desktop                                           # React
npm run build --prefix src/Nwflash.Desktop                                          # 生产前端构建
cd src/Nwflash.Desktop && npm run tauri -- build --no-bundle                        # Tauri 二进制（慢）
```

## 阶段记录

### Task 1 完成（共享设备身份 helper）
- 新增 `commands/device_identity.rs`：`read_online_ota_identity` / `online_ota_identity_from_process_output` / `online_ota_identity_from_getprop`，从 safe_flash.rs 移出。解析语义不变（PD=`ro.product.device`；版本=`ro.build.version.bbk` 末段回退链）。
- safe_flash.rs 删私有实现 + 冗余测试。nwflash-tauri 114 测试绿 + mapping gate 绿。
- 分支 `feat/root-ota-cloud-extract`，已提交。
- 计划：`docs/superpowers/plans/2026-08-18-vivo-root-cloud-extract.md`（Task1-7）。
- 注意：git 提交时有 LF/CRLF 规范化导致整文件 diff（提交显示 1148 insertions），内容无问题，后续正常。

### Task 3 完成（RootOtaService 编排）
- 新增 `application/src/root_ota.rs`：`RootOtaService::extract` 探测格式 → payload 分支(复用 FirmwareExtractService URL 直传)/直接镜像 zip 分支(复用 infra extract_zip_members)。
- boot 槽位 init_boot 优先、boot 回退；产出 `RootOtaExtractedImages { boot_image, boot_partition_name, vendor_boot, staging_root }`。
- `create_staging_root()` 唯一 staging（`nwflash-root-ota/{pid}_{ms}_{seq}`）。
- 7 集成测试全过（本机 Range server）。**坑：reqwest 发小写 `range:` 头，mock 必须大小写不敏感解析 Range**（否则返回 200→RangeUnsupported）。
- 已提交。

### Task 4 完成（boot/init_boot 分区名穿通）
- `RootImageSelection` +`target_partition_name`；`RootImageRuntime::replace(kind,img,target)` + `replace_default`（本地用 kind.label()）+ `get_boot_with_target`。
- `RootPatchedArtifactRuntime::replace/replace_owned` 显式 `flash_partition: QuickFlashPartition` 参数。
- `quick_flash_partition_from_name`（boot/init_boot/vendor_boot 映射）。
- `build_vivo_ksu_patch_commands` 参数化 partition（脚本内用相对 basename，push/pull 用绝对路径），白名单校验。
- 移除 `RootImageKind::quick_flash_partition`（改由名字派生）。116 测试绿，workspace 全绿。

### Task 5 完成（root_ota 命令 + RootOtaRuntime + 接线）
- 新增 `commands/root_ota.rs`：`root_ota_check`（读 PD/版本 → resolve_rom → 缓存 URL 留 Rust）/ `root_ota_extract_images`（云提取 → 灌 RootImageRuntime）。
- `RootOtaRuntime`：绑定 serial（换设备失效）、staging 成功期交给 runtime / 失败清理；`session_stop` 清理。
- `safe_flash::session_token` 改 `pub(crate)` 共享。命令 DTO 无 URL/serial/path。
- AppState 加 `root_ota_runtime`；generate_handler 注册 2 命令。118 测试绿 + workspace 全绿。

### Task 6 完成（前端 UX + vitest）
- RootPage.tsx 加「OTA 来源」单选（本地/从服务器）+ 检测按钮；打开自动检测（root_ota_check）；勾选服务器 → root_ota_extract_images 灌槽位。
- 本地选择按钮在服务器模式禁用；根因：refresh 新增 root_ota_check 调用需给每个测试 mock 链加一项（否则 Once 序列错位）。已用 OTA_CHECK_* 常量重写 RootPage.test.tsx。
- 130 前端测试绿 + build 成功。

### Task 7 完成（全量验证）
- Rust workspace 全绿；e2e feature 编译通过 + C# 映射门禁绿；前端 130 绿；生产 build 成功。
- 完整 WDIO 启动套件需桌面+显示，本环境headless 无法跑；以 e2e feature 编译 + 无设备门禁为准（符合验收约束）。

### 后续待办（全局审查 + 文档 + 清理 + UI 优化 + 新功能提案）
- [ ] 功能 BUG 自审（云提取跨格式边界/分区名穿通/取消/资源清理）
- [x] 独立代码审查（root.rs Critical #1 / remote_firmware.rs Important #2 / 进度 Important #3）
- [x] 修复 Critical #1：`automatic_root_flash_source` 硬编码 init_boot → 遍历 InitBoot/Boot/VendorBoot + boot 回退校验 + 回归测试（未提交）
- [x] 修复 Important #2：取消误报 Archive → `io::ErrorKind::Interrupted → Cancelled` + 中途取消测试（未提交）
- [x] 修复 Important #3：进度透传 `root_ota_extract_images` → `RootOtaService::extract` → `report_progress_monotonic`（payload 分支算总字节 fraction；直接镜像 zip 分支 list_zip_members 算 total + 跨成员累计）（未提交）
- [ ] 提交审查修复 + 重跑全量测试
- [x] 项目架构文档更新（`project-architecture.md` / `rust-tauri-architecture.md` 已补 ROOT 云提取边界）
- [x] 清理临时文件（已删除 `%TEMP%\\probe_zip.py` 与 `%TEMP%\\zip_member.py`；不删除受校验 payload 缓存）
- [ ] Rust 代码漏洞/安全审查与修复
- [ ] taste-skill UI 优化
- [ ] 新功能提案文档（docs/superpowers/plans/ 或 docs/proposals/）

### 关键技术落点（实现时对照）
- infra `Cargo.toml` reqwest 需加 `blocking` feature（`RangeHttpReader` 用同步 client）。
- `RangeHttpReader`：Read+Seek 走 HTTP Range，CHUNK=1MB 填充缓冲，seek 清缓冲；new 时 `Range: bytes=0-0` 要 206 + Content-Range 总长，非 206 → `RangeUnsupported`。取消检查在每次网络拉取前，返回 io::ErrorKind::Interrupted。
- 探测：首 4 字节 `PK`→zip（再用 zip crate 查 payload.bin→PayloadZip/DirectImageZip）、`CrAU`→PayloadRaw、`1f 8b`→Unsupported。
- 直接镜像 zip 提取：`ZipArchive::new(RangeHttpReader)` + 按基名匹配 wanted + 手动 buffer 循环注入进度 + CRC/大小校验。
- 测试：写 `tests/common/mod.rs` 的 `spawn_range_server(data)->String`（std TcpListener，解析 Range 返回 206/Content-Range/切片）。

### 最新复查（2026-08-18，待提交）

- 修复直接镜像 ZIP 的进度传播：infra 回调改为 `FnMut`，application 按成员名计算增量并实时上报；新增 `direct_zip_reports_monotonic_fractional_progress_until_completion` 回归测试。
- 修复直接镜像 ZIP 的不必要工具依赖：命令先在 Rust 阻塞线程探测 OTA；仅 `PayloadZip`/`PayloadRaw` 运行 `PayloadDumperProvisioner`，直接镜像 ZIP 不再因 payload 工具下载失败而无法提取。
- 收紧 ROOT OTA 的错误投影：网络、归档、完整性和 URL 原始错误不再把 OTA URL 或 staging 路径传给 React；新增回归断言。
