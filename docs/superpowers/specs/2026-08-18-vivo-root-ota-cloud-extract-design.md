# Vivo ROOT 云端 OTA 提取设计（boot/vendor_boot 云提取）

日期：2026-08-18
状态：approved（用户已确认架构总览 / 云端源获取 / 云提取核心；分区名穿通采用「boot 回退 + 分区名穿通」）

## 背景与目标

Vivo 线刷（SafeFlash）已经能「服务器获取 OTA → 下载整包 → 本地解包 → fastbootd 刷入」。Vivo ROOT（KernelSU 启动镜像修补）目前只支持本地选镜像。本设计让 ROOT 页获得同源能力，但走**云提取**：服务器解析出 OTA 链接后，客户端**不下载整包**，而是按需用 HTTP Range 只拉取修补所需的启动镜像（init_boot/boot/vendor_boot）作为「待修补文件」，后续 KSU 修补与刷写流程与现有完全一致。

### 实测依据（2026-08-18，用户提供两个真实 OTA 链接）

| 链接 | 格式 | 提取方式 | 实测结果 |
|---|---|---|---|
| OTA1（9.3GB，PD2417） | payload OTA（zip 含 payload.bin） | `payload_dumper.exe <URL> -i init_boot,vendor_boot` | 9.8s 提取 init_boot.img(8MB)+vendor_boot.img(128MB)，不下载整包 |
| OTA2（9.4GB，PD2183） | 直接镜像 zip + 块式内容（无 payload.bin） | zip 中央目录 Range 解析 + 成员定向抓取（ZIP64 + CRC 校验） | ~1s 各提取 boot.img(96MB)/vendor_boot.img(64MB) |

关键发现：
- OTA1 payload 内同时有 `init_boot` 与 `boot`；OTA2 只有 `boot` + `vendor_boot`，**没有 `init_boot`**。
- `payload_dumper`（payload-dumper-rust，捆绑于 `xxz13352/NWFlash` v1.0.0，SHA-256 固定）的 PAYLOAD 参数**直接接受 URL**：本地 .bin / .zip（含 payload.bin）/ 远程 URL 均支持，远程时只按需 Range 读取，不整包下载。`-i/--images` 过滤分区，`-m/--metadata` 列分区。
- 两个 OTA 均为 ZIP64、支持 HTTP Range（206）、服务端需自定义 User-Agent 才返回有效内容。

## 范围

- **新增**：ROOT 页「从服务器获取」单选（仅在服务器解析到 OTA 时出现）；远端 OTA 格式探测；payload OTA 的 URL 直传提取；直接镜像 zip 的 Range 成员定向提取；boot/init_boot 分区名穿通。
- **复用**：`/api/rom`（resolve_rom）、`payload_dumper` provisioner、`FirmwareExtractService::inspect_payload`/`extract_payload_with_expected_sizes_and_progress`、`OperationCoordinator`、`RootImageRuntime`、现有 KSU 修补与刷写流程。
- **不实现**：Vivo gzip 固件（服务器 OTA 是 zip，不涉及）；块式分区重建（ROOT 不需要 system/vendor）；服务端云端解包（Cloudflare Worker 无法跑 payload_dumper，且实测客户端 Range 提取已足够）。

## 架构

完全沿用现有分层，不加新 crate：

```
RootPage 打开 ──> root_ota_check ──> 读设备 PD/版本 ──> client.resolve_rom(/api/rom) ──> 缓存 OTA(URL 留 Rust 内存) ──> 返回 {available,label}
    │ 勾选「从服务器获取」
    └──> root_ota_extract_images ──> 用缓存 URL ──> 远端格式探测
                                              ├─ payload OTA → 复用 FirmwareExtractService（URL 直传 payload_dumper, -i 分区）
                                              └─ 直接镜像 zip → 新 RangeHttpReader + zip crate 定向抓取 init_boot/boot/vendor_boot
             └─> 产出镜像 → feed RootImageRuntime（不透明 ID）→ 现有 preflight/patch/auto 流程原样复用
```

### 涉及 crate

- `nwflash-infrastructure`
  - 新增 `remote_firmware.rs`：远端格式探测、`RangeHttpReader`（Read+Seek，Range 请求+小缓存，ZIP64 支持）、直接镜像 zip 成员定向提取。
- `nwflash-application`
  - 新增云端提取编排（payload URL 直传路径复用现有 `FirmwareExtractService`；zip 成员路径编排 `remote_firmware`）。
- `nwflash-tauri`
  - `commands/root.rs`：新增 `root_ota_check`、`root_ota_extract_images`；抽共享的 `read_online_ota_identity`（当前是 `commands/safe_flash.rs` 私有函数，移动为共享 helper）。
  - 新增 `RootOtaRuntime`：持有解析结果缓存 + 提取 staging 所有权。
- `src/pages/RootPage.tsx`：来源单选 + 检测按钮 + 提取后状态填充。

## 云端源获取（root_ota_check）

- 页面打开时若 `has_token` 且有 ADB 设备：调用 `root_ota_check`。Rust 内读设备 PD/版本（现有 `read_online_ota_identity` 逻辑：`ro.product.device` + `ro.build.version.bbk` 末段及回退链）→ `resolve_rom(token, pd, version)` → 成功则缓存 `ResolvedRootOta { url, name, size_bytes, pd, version }` 到 `RootOtaRuntime`，返回 `{ available: true, label }`；读取设备信息所需的 serial 仅是当前命令的临时参数，不写入 runtime；否则静默返回 `available: false`（不打断页面）。
- 手动「检测服务器 OTA」按钮复用同一 command。
- **URL 绝不进浏览器**：前端只拿 `available` + 显示 label + 不透明镜像 ID。
- 缓存不绑定 `serial`；提取直接使用当前唯一设备和受控 OTA 来源，不做预检/提取 serial 比较。serial 只在 Rust 构造当前 ADB/Fastboot 命令时临时派生，允许作为该命令的瞬态目标参数，但不进入 DTO、OTA runtime 或持久化身份。
- 信用点成本：唯一点一次 `/api/rom`（检查时），提取复用缓存，不重复查询。

### Data Transfer Object（前端可见）

```jsonc
// root_ota_check 返回
{ "available": true, "label": "PD2417_16.2.12.0.W10.V000L1 OTA" }
// 或
{ "available": false }

// root_ota_extract_images 返回（全部为不透明 ID / 安全摘要，无 URL/serial/路径）
{
  "sourceLabel": "已从 PD2417 OTA 提取",
  "initBoot":  { "id": "root-image-init_boot-3", "kind": "initBoot", "fileName": "init_boot.img", "sizeBytes": 8388608 },
  "vendorBoot": { "id": "root-image-vendor_boot-4", "kind": "vendorBoot", "fileName": "vendor_boot.img", "sizeBytes": 134217728 }
}
```

`initBoot`/`vendorBoot` 结构与现有 `root_select_image` 的返回值一致，可直接复用现有 state。

## 云提取核心（root_ota_extract_images）

在 `OperationCoordinator` 内运行（可取消、有进度、日志、互斥）。

### 远端格式探测

HTTP Range 拉首 4 字节（仿 WPF `FirmwareFormatDetector.PeekAsync`）：
- `PK` → zip；再开一次中央目录（Range）检查是否含 `payload.bin` → 分支 A（payload OTA）或分支 B（直接镜像 zip）。
- `CrAU` → 裸 payload.bin → 直接分支 A。
- 其它 → 「不支持的 OTA 格式」。

### 分支 A — payload OTA

把 OTA URL 直接传给现有 `FirmwareExtractService`（payload_dumper 支持 zip URL / 裸 URL，已实测）：
1. `inspect_payload(exe, url, metadata_dir, cancel)` 列分区（payload_dumper `--metadata`，只 Range 拉 manifest）。
2. 用已有过滤逻辑选目标分区：`init_boot`（优先）或 `boot`（回退）+ `vendor_boot`（若存在）。
3. `extract_payload_with_expected_sizes_and_progress(exe, url, selected, staging, cancel, progress)` 提取，进度/取消/大小校验全部复用。
4. 全程零新增解包代码。

### 分支 B — 直接镜像 zip

新增 `RangeHttpReader`（`std::io::Read + Seek`，Range 请求 + 少量缓存，支持 ZIP64，报告总长度）+ 现有 `zip` crate（v4.2.0，已在 nwflash-infrastructure 依赖）：
1. `ZipArchive::new(RangeHttpReader)`，读取中央目录（只有几十 KB）。
2. 按文件基名匹配 `init_boot` / `boot` / `vendor_boot` 成员（`.img` 后缀）。
3. 定向解压（deflate/store，CRC 由 zip crate 校验）到 staging，按解压字节报进度。
4. 块式内容（`system.new.dat.*`、`*.transfer.list` 等）与本功能无关，忽略。

### 产出与 staging 生命周期

- 提取结果 `init_boot`（或回退 `boot`）进 `RootImageRuntime` boot 槽位 + `vendor_boot` 进 vendor 槽位，均为不透明 ID；缺失的分区按现有逻辑判可用性。
- 提取 staging 由 `RootOtaRuntime` 持有并负责清理：二次提取/再次检测时替换并清理旧 staging；会话结束（`session_stop`/登出）时清理。提取出的源镜像文件在修补命令读取前必须保持存活，因此不随提取命令返回即清理。

## 分区名穿通（boot/init_boot）—— `commands/root.rs`

- `RootImageSelection`（Rust runtime）新增 `target_partition_name: String`；本地手动选镜像默认 `init_boot`/`vendor_boot`；云端提取按实际来源设为 `init_boot` 或 `boot`。
- `build_vivo_ksu_patch_commands` 的 `--partition {name}`、远端上传/拉回文件名（`vivoksu_{name}.img`）改用该名字；补丁产物刷写目标分区按真实名派生 `QuickFlashPartition`（`boot→Boot`、`init_boot→InitBoot`）。
- 受影响 Rust 单测同步参数化更新（命令参数断言按真实分区名）。
- vendor_boot 槽位保持 `vendor_boot` 不变（两种 OTA 中该分区名均一致）。

## 前端 UX（RootPage.tsx）

- `sourceMode: 'local' | 'server'` 单选（default local）；`otaAvailable`/`otaLabel` 由 `root_ota_check` 填充。
- 工作台新增「OTA 来源」行：「本地镜像」/「从服务器获取」（后者仅当 `otaAvailable` 时可选）+「检测服务器 OTA」按钮。
- 勾选「从服务器获取」→ 调用 `root_ota_extract_images` → 返回的 `initBoot`/`vendorBoot` 不透明选项塞进现有 state → 现有 preflight/修补/全自动按钮原地复用，零流程改动。
- 前端不接触 URL / serial / staging 路径，只显示「已从 {OTA label} 提取 init_boot」等安全摘要。

## 错误处理

沿用 sanitize 风格，不外泄 URL/串号/路径：
- 无设备 / 未登录 → 不可用态（`available:false`）。
- OTA 非 payload 且无 boot/init_boot → 「该 OTA 不含 ROOT 所需分区，请换源或选本地镜像」。
- differential（增量）OTA → 「增量 OTA 暂不支持云提取」。
- 未知格式 / 网络失败 / 服务端错误 → 安全分类错误（映射到 `sanitize` 语义，同 SafeFlash）。

## 安全边界

- URL / serial / pd / version / staging 路径全部在 Rust 侧；DTO 只含不透明 ID + 显示标签 + 大小。
- `RootOtaRuntime` 不保存或绑定 serial；换设备不会触发 serial 等值校验，当前命令始终从最新唯一设备快照取得临时目标。多设备发现仍直接返回 `MultipleDevices` 拒绝态。
- command 入参 `deny_unknown_fields`，拒绝浏览器伪造字段（仿 `RootPreflightOptionsDto`）。
- 无新前端能力：浏览器只提交意图（`check`/`extract`）+ 已有不透明 ID。

## 测试计划

- `RangeHttpReader` 单测：ZIP64 / 非 206 服务器 / 损坏 CRC / 缺分区 / 段越界，全部本机 mock（遵循「全部测试不需要真实 token 或设备」纪律）。
- 直接镜像 zip 成员提取：录制 fixture 覆盖 init_boot+ boot 回退 + vendor_boot 缺失。
- payload URL 路径：复用现有 payload_dumper 单测 + 录制输出；断言 URL 直接传给 payload_dumper（不经本地下载）。
- 分区名穿通：`build_vivo_ksu_patch_commands` 按 boot/init_boot 参数化断言；flash 计划目标分区正确。
- Command DTO 安全：`root_ota_check`/`root_ota_extract_images` 的序列化结果不包含 URL/serial/path。
- 前端 vitest：radio 出现/禁用/勾选触发 `root_ota_extract_images`/提取结果填充、路径安全断言。

## 未完成边界（后续项）

- OTA 链接签名过期（`sign/t` 参数）导致提取失败：报服务器可重试错误，不自动重试。
- `boot`/`init_boot` 之外的平台（如 `vendor_boot` 缺失的单分区设备）不在本次范围。
