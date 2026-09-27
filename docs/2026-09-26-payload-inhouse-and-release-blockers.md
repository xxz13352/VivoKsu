# payload 提取内建化 + 发布链遗留问题

日期：2026-09-26
提交：`a5dea00`（payload 提取内建化）、`8e44045`（驱动安装修复）

## 一、payload 提取已内建化

外部 `payload_dumper.exe` 换成 `nwflash-infrastructure/src/payload/` 的内建
Rust 实现。理由与实现要点见提交信息，此处只记结论：

- 进度来自解压循环内部，是真实字节级；120 秒无进展判死随之删除
- 支持 op 类型 0/1/6/8/14（含 Android 12+ 的 ZSTD）
- 远程 URL 按 Range 直读，含 ZIP64 定位

## 二、发布链上的遗留问题（**非本次改动引入**）

### 2.1 `magiskboot.so` 的资源哈希对不上

`scripts/New-TauriReleaseManifest.ps1` 会逐条校验 `packaging/release/tauri-resources.json`
里的 SHA256，当前在第一条不一致处就抛错：

```
Bundled resource integrity mismatch: resources/root-tools/magiskboot.so
```

实测对比：

| 项 | 值 |
|---|---|
| `resources/root-tools/magiskboot.so` 实际 SHA256 | `a2b14ec3b5c953ffd519ab6f944de346f564c6a2bcb944c6f056ed10a319a4fb` |
| manifest 声明 | `d7440e2cd89899426e809554bf793baef9804ccbe5a52ce34a8b6242725d3c77` |
| 文件大小 / 修改时间 | 769600 字节 / 2026-09-18 18:19 |

**这是既有问题**：`git show HEAD~1` 能证明我改动之前两者就已经不一致
（文件名相同、manifest 哈希不变，但文件内容对不上）。

**影响**：`Test-TauriRelease.ps1` 及所有依赖它的发布门禁**现在就会失败**，
与 payload 改动无关。

**需要人工决定**：是文件被有意替换（那就更新 manifest 里的哈希），还是文件
被误改（那就恢复原文件）。**不宜由工具自动挑选一方**——这两者含义完全相反，
自动「以文件为准」会把一次可能的篡改固化成发布基线。

其余 26 条资源哈希**全部一致**，缺失 0 条。

### 2.2 发布产物变化

移除 `payload-tools/payload_dumper.exe` 后：

- `tauri.conf.json` 的资源声明：11 → 10 条
- `tauri-resources.json`：28 → 27 条（`payload-tools/` 条目已删）
- `scripts/Upload-Resources.ps1`：不再打包/上传 `payload_dumper-win-x64.zip`
- `tests/build_smoke.rs`：不再断言该资源存在

发布前必须重新生成资源清单，并确认远端 release 资产列表同步（否则旧的
`payload_dumper-win-x64.zip` 会留在 GitHub release 上成为孤儿资产）。

## 三、当前可验证状态

| 门禁 | 结果 |
|---|---|
| `cargo test --workspace` | 1101 通过 / 0 失败（62 套件） |
| `cargo build --workspace` | 0 告警 |
| `rustfmt --check` | 干净 |
| 前端 `npm test` | 261 通过 |
| 真实固件远程解析 | 38 分区，偏移 61，与实测字节一致 |
| `New-TauriReleaseManifest.ps1` | **失败（见 2.1，既有问题）** |

## 四、仍未闭合的发布环节

按 `docs/release/tauri-vmp-signing-runbook.md`，以下仍需人工执行：

1. VMProtect Lite GUI 处理交接包（仓库脚本刻意禁用自动化）
2. 代码签名（需 `NWFLASH_CERT_THUMBPRINT`）
3. NSIS 打包与安装/卸载验证
4. 真机矩阵
