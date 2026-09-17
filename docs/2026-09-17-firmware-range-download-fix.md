# 2026-09-17 固件 Range 下载断流修复

更新时间：2026-09-17（Asia/Shanghai）
范围：`nwflash-infrastructure` 的固件 HTTP Range 读取链（线上下载 + 云提取）
关联：审计 A33、A34（本轮一并关闭超时/停滞检测部分）

## 报障与现象

用户报障（09-16）：**每次** vivo 线刷下载固件都失败。

```
失败详情：固件下载失败。Range下载过程中断：error decoding response body
```

「每次」是关键信息——排除偶发网络抖动，指向确定性行为。

## 定位

1. 该文案唯一来源是 `ota_download.rs` 的 `download_range_segment_inner`
   中 `response.chunk()` 的错误分支，即**并行 Range 分片拿到响应头之后、
   正文中途被断开**。HEAD 探测、206 状态、`Content-Range` 三元组校验全部
   通过，因此不是鉴权、不是「服务器不支持分段」。
2. `error decoding response body` 是 reqwest 对**响应体解码失败**的顶层描述
   （`Kind::Body`），底层 hyper / rustls / IO 原因在 `source()` 链里。原实现
   直接 `{error}` 打印，把真实原因丢掉，导致「连接被重置」「正文提前结束」
   「TLS 未发 close_notify」三类完全不同的故障不可区分。
3. reqwest 特性已核对：`default-features = false`，仅 `json` / `rustls-tls` /
   `blocking`。**无 gzip/brotli**（排除自动解压问题）、**无 http2**
   （排除 h2 `RST_STREAM`），走 HTTP/1.1。
4. Vivo 固件 CDN 指纹（实测 `curl --noproxy '*' -D - https://sysuptxdl.vivo.com.cn/`）：

   | 响应头 | 含义 |
   | --- | --- |
   | `Server: Byte-nginx` + `X-Oss-*` | 火山引擎 TOS 源站 |
   | `X-NWS-LOG-UUID`、`Via: cacheXX.<节点>` | 网宿 CDN |
   | `X-Bdcdn-Cache-Status` | 百度 CDN |

   多级 CDN 架构。根路径 403、`/upgrade/` 空响应、官网下载链接经 ajax 取签名 URL，
   拿不到可复现的公开直链，因此改用本地 TCP fixture 精确复现（见下）。

## 根因

原实现的分片重试策略与 CDN 的实际行为不匹配：

- 每个并行分片是**一次性请求整个分片**（数 GB ÷ 连接数，单请求数百 MB）；
- 断流后「从分片起点重下」，只重试 3 次且**零退避**；
- 结果：CDN 只要稳定地截断长响应，3 次重试必然全败 → 整个下载判死。

这与 C# 基线不一致。C# 用的 bezzad `Downloader`（`OtaDownloadService.cs`）是按
1 MB `Packet` 步进推进、失败后**续传该分片剩余部分**（`EnableAutoResumeDownload`），
天然不受「单请求被截断」影响——移植到 Rust 时丢掉了这个能力。

同一故障类还波及第二条链：`remote_firmware.rs`（**固件提取**页的云提取 Range 读取）
单次请求、无重试、无超时，且 `validate_range_response` 只在「短范围正好是文件尾」
时才接受（`end + 1 != total_len` 即判 `RangeUnsupported`），会把 CDN 截短范围
**误报成「服务器不支持 Range」**。

## 改动

### `ota_download.rs`（线上固件下载）

| 改动 | 说明 |
| --- | --- |
| 分片内**子块续传** | 按 `OTA_RANGE_SUBREQUEST_BYTES = 4 MiB` 逐子块请求；断流只损失当前子块，有字节落盘就从**已落盘偏移**继续，不重下 |
| 零进展才退避重试 | 有字节落盘即重置失败计数；连续 5 次零进展才判本分片失败。退避 400ms→3.2s，`sleep_with_cancellation` 期间可取消 |
| 并发整体失败 → 退化单连接 | RangeParallel 计划因纯网络错误整体失败时，用 `connections = 1` 重下一次（删 staging 重来），CDN 拒绝并发 Range 时不再直接判死 |
| `ensure_range_response` 接受更短范围 | 按 RFC 9110 允许服务端只返回请求范围的前一段，返回实际字节数供调用方推进；Content-Range 改为结构化解析 |
| 停滞检测 + 建连超时（A34） | `build_ota_http_client()` = `connect_timeout(20s)`，不设总超时；每次 `response.chunk()` 外包 `timeout(60s)`，超时即中断本次请求交给续传 |
| 错误文案带原因链 | `describe_http_error` 展开 `source()`，输出形如 `error decoding response body ← connection closed before message completed` |

去掉旧的「重试回退全局进度」逻辑——续传下每字节只写一次，无需回退。

### `remote_firmware.rs`（云提取 Range 读取）

| 改动 | 说明 |
| --- | --- |
| `default_client()` 加超时（A33） | `connect_timeout(20s)` + `timeout(60s)` |
| 新增 `fetch_range_bytes()` | 4 MiB 逐窗口 + 断流续传 + 零进展退避重试（×5），`RANGE_MAX_REQUESTS = 4096` 失控保护 |
| `fetch_range_window()` 返回 `(字节, Option<错误>)` | 两者**可同时非空**——服务器发一半才断开时那一半仍有效。协议违规（非 206 / 范围不符 / 正文超声明长度）标 `RangeUnsupported` 且**不重试**，只有 `Transport` 才重试 |
| `validate_range_response` 接受更短范围 | 去掉 `end + 1 != total_len` 的拒绝，由续传补齐 |
| `RangeHttpReader::fetch_from` 改用上述 helper | 短读由 `Read` 实现按新位置继续补拉 |

**关于阻塞 reqwest 的超时语义（关键事实）**：阻塞 `ClientBuilder` **没有**
`read_timeout`，但 `blocking/response.rs` 的 `impl Read for Response` 是
`wait::timeout(body.read(buf), self.timeout)`——即 client `timeout` 被施加到
**每一次 `Read::read`** 上。所以 `timeout(60s)` 的语义是「60 秒收不到新字节就
中断本次读取」的停滞检测，而不是整个响应的总时限，慢速但持续有数据的传输不会
被误杀。

### `firmware_extract.rs`（远程格式探测）

三处 `远程固件读取失败：{error}` 改用 `describe_error_chain`，与上面两条链的
可诊断性对齐。该探测是 4 字节 Range + 10s 超时，本来就有时限，未加续传。

## 新增回归测试

复现 fixture：`tests/common/mod.rs` 新增
`spawn_unreliable_range_server(data, truncate_after, fail_first)`：

- `truncate_after = Some(n)`：206 照请求范围声明 `Content-Range` / `Content-Length`，
  但只发送前 `n` 字节就断开——精确复现用户看到的 `error decoding response body`；
- `fail_first = k`：前 `k` 个 Range 请求只回响应头、零字节正文就断开。

| 测试 | 断言 |
| --- | --- |
| `truncated_range_response_resumes_from_the_bytes_already_on_disk` | 512 KiB 文件、每次只给 37 KiB，下载必须成功且字节完全一致 |
| `exhausted_parallel_ranges_fall_back_to_a_single_connection` | 前 10 个请求零进展 → 并行计划耗尽后必须退化单连接成功 |
| `probe_kind_survives_a_truncated_range_response` | CrAU 4 字节魔数、每次只给 2 字节，探测必须靠续传补齐 |
| `truncated_range_responses_are_resumed_while_extracting_zip_members` | zip 每次响应截断到 8 KiB，中央目录与成员字节靠续传补齐，且 **zip crate 的 CRC 校验必须通过**（证明补齐字节完全正确） |

## 验证证据

命令（Windows + Git Bash，需先导出 MSVC 环境，见 `rust-windows-msvc-build` 技能）：

```bash
cargo test -p nwflash-infrastructure --offline
cargo test -p nwflash-application --offline
cargo check --workspace --all-targets --offline
rustfmt --edition 2021 --check <改动文件>
```

| 项 | 结果 |
| --- | --- |
| `nwflash-infrastructure` | lib 105/105；`ota_download` 19/19（含 2 条新回归）；`remote_firmware` 17/17（含 2 条新回归）；其余集成测试全部通过；0 warning |
| `nwflash-application` | 全部测试二进制通过，0 failed |
| `cargo check --workspace --all-targets` | `Finished`，0 error 0 warning |
| `rustfmt --check` | 6 个改动文件通过 |

## 风险与未覆盖

- **未经真实 CDN 复现**。公开直链不可得（签名 URL + 需登录的 `/api/rom`），
  验证基于本地 fixture 精确复现故障形态。修复的价值在于**不依赖断流的具体原因**：
  子块续传让任意原因的截断都只损失当前子块。
- **并发 Range 被拒**这一类只靠「退化单连接」兜底，未定位 CDN 的并发上限。
  若用户复现仍有问题，新的错误文案会直接给出 hyper/rustls 层原因。
- `firmware_extract.rs` 的 4 字节探测未加续传（有时限，且 4 字节被截断的概率极低）。
- 真机/真 CDN 验收仍需补：建议在 `device-acceptance-matrix` 里补一条
  「弱网/多 CDN 场景下完整下载一次数 GB OTA」的验收项。
