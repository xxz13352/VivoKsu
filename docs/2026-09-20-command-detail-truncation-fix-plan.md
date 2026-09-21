# 改进方案：命令留痕不再丢掉「失败原因」（截断策略）

- 提出时间：2026-09-20（Asia/Shanghai）
- 触发：run `v1:440` 的 `flash system` 明细只存了前 300 字符，真正报错在尾部，D1 里查不到
- 结论先行：**本次 440 的尾部永久丢失，无法从任何库恢复**；下面是让以后不再丢的方案

## 1. 事实与证据

| # | 证据 | 结论 |
|---|---|---|
| 1 | `usage_logs.id=440` 的 `details_json` 第 101 行 = 前缀 77 字符 + stderr **恰好 300 字符** + `…`，合计 378 | 300 是**客户端写死的每流上限**，不是 D1 / 服务端截断 |
| 2 | 写入端：`nwflash-application/src/command_detail.rs:30` `DEFAULT_OUTPUT_CHARS = 300`；`:191` `clip()` 取 `chars().take(limit)` + `…` | 取的是**头部**，尾部整段丢弃 |
| 3 | `usage_operation_runs` / `usage_operation_events` / `usage_output_chunks` 三表全 **0 行** | V2 全文分块通道没启用（该用户 268 条运行全部走 V1） |
| 4 | 本机 `%LOCALAPPDATA%\Nwflash\operations.log`：1508 行、**`[cmd]` 0 行**、末条 2026-09-19 10:37 | 命令输出按设计**不进本地日志**；且 20:30–21:10 本机无任何 nwflash 应用文件写入 |
| 5 | `nwflash-windows/src/process.rs` 的文件式 stdout 捕获只有一个生产调用点（`commands/partitions.rs:798`，分区回读） | `flash` 输出只在内存里，用完即弃 |

**缺口有多大**：`flash system` 是 256 MB sparse 镜像，fastboot 的结论只在最后两三行（`FAILED (remote: '…')` / `fastboot: error: Command failed`）。sparse 进度刷屏稳定吃掉整个 300 字符预算 → **明细永远只看到「传到 5%」**。

## 2. 影响面（两处，不止一处）

| 位置 | 现状 | 后果 |
|---|---|---|
| `command_detail.rs::clip` | 保留头部 300 字符 | 服务端明细丢尾部错误 |
| `safe_flash.rs::fastboot_failure_summary_with`（`:886`，`MAX_LOG_BYTES = 2000`） | 保留头部 2000 **字节** | **客户端失败弹窗也丢尾部错误** —— 用户当场看到的就是「传到 5%」，查不到原因 |
| `nwflash-windows/src/process.rs` 文件式捕获 | 仅分区回读在用 | 与本问题无关，可不动 |

即：现在**服务端和本地弹窗同时看不到失败原因**，这才是「排障无据」的根因。

## 3. 方案 A（推荐）：头部 + 尾部，总长不变

思路：单流预算仍是 300 字符，但**按比例分给头和尾**，把「在做什么」和「为什么失败」都留下。

`nwflash-application/src/command_detail.rs`

```rust
/// 单条命令留痕里 stdout / stderr 各自的字符上限（不变）。
pub const DEFAULT_OUTPUT_CHARS: usize = 300;

/// 截断时头部占比 2/5：fastboot/asr 的失败原因在末尾，sparse 进度刷屏必然占满头部，
/// 所以 300 字符里给头 120、给尾 179，头尾之间用 `…` 连接。
const HEAD_SHARE_NUM: usize = 2;
const HEAD_SHARE_DEN: usize = 5;

fn clip(text: &str, limit: usize) -> String {
    let total = text.chars().count();
    if total <= limit {
        return text.to_string();
    }
    if limit < 16 {
        // 极小上限保持旧语义，避免出现「头 0 + 尾负数」。
        let mut clipped: String = text.chars().take(limit).collect();
        clipped.push('…');
        return clipped;
    }
    let head = limit * HEAD_SHARE_NUM / HEAD_SHARE_DEN;
    let tail = limit - head - 1; // 1 个字符留给 '…'
    let mut out = String::with_capacity(text.len().min(limit * 4) + 3);
    out.extend(text.chars().take(head));
    out.push('…');
    out.extend(text.chars().skip(total - tail));
    out
}
```

`nwflash-application/src/safe_flash.rs`

```rust
const MAX_LOG_BYTES: usize = 2000;
/// 失败摘要同样以尾部为准：真正的原因（FAILED／no link）在最后几行。
const SUMMARY_HEAD_BYTES: usize = 500;
const SUMMARY_TAIL_BYTES: usize = 1500;
// combined.len() > MAX_LOG_BYTES 时：
//   head = 对齐 UTF-8 边界的 combined[..500]
//   tail = 对齐 UTF-8 边界的 combined[len-1500..]
//   拼接为 "{head}\n…（中间日志已省略）\n{tail}"
//   仍不足 2000 字节时整段返回（旧行为）。
```

要点：
- **不改上行体积**：单流仍 ≤ 300 字符 + `…`；服务端 schema / D1 / 管理台零改动。
- 头尾都要**按 char 边界切**（`clip` 用 `chars()` 天然安全；`fastboot_failure_summary_with` 用字节，必须沿用现有 `is_char_boundary` 回退逻辑，尾部向前对齐）。
- V1 明细与 V2 trace 共用 `render_stream`，一处改动两边受益。

## 4. 测试要点

| 文件 | 用例 | 断言 |
|---|---|---|
| `command_detail.rs` | 改既有 `truncates_long_output_but_marks_it` | 头 120 与尾 179 都在；中间出现 `…` |
| `command_detail.rs`（新增） | `keeps_tail_failure_line_for_chatty_flash` | 10 MiB 假输出 + 末尾 `FAILED (remote: 'no link')` → 结果 **contains** 该行 |
| `command_detail.rs`（新增） | 不变量：输出结果字符数 == `limit + 1`（或按新定义 ≤ `limit + 1`） | 防以后误改配比导致体积膨胀 |
| `safe_flash.rs` | 改 `fastboot_failure_summary_hides_private_keys_and_truncates_utf8_safely` | 尾部错误可见 + 结果仍是合法 UTF-8 |
| `safe_flash.rs`（新增） | 尾部落在多字节字符中间 | `from_utf8` OK，无 replacement char |

## 5. 备选方案对比

| 方案 | 改动量 | 收益 | 代价 |
|---|---|---|---|
| **A 头 120 + 尾 179（推荐）** | 2 文件、2 个函数 | 失败原因必在；体积不变 | 成功命令的头部上下文变少 |
| B 全给尾部 300 | `clip` 1 个函数 | 实现最简，错误必在 | 丢掉「在做什么」的开头 |
| C 上限抬到 2000（头 500 + 尾 1500） | 常量 + 配比 | 信息量最大 | details_json 变大；100 条明细可能触服务端归一化上限（`trace-v2-query.ts` 注释里的 500 条 / 16 KiB），需先评估 |
| D 改走 V2 全文分块 | 新建 producer → `trace_spool`（1 MiB / 200 块 / 7 天）→ `usage_output_chunks` | 近似全文可回看 | 独立特性；仍非无限（200 块 × 1 MiB），且要接通 V1→V2 迁移 |

若最终要「完整日志」而非「完整错误」，只有 D 能做到接近，且必须在客户端侧分块落盘（服务端 D1 不可能装 256 MB）。

## 6. 验收判据

1. `cargo test -p nwflash-application command_detail` 全绿，含新守卫用例。
2. 复现一次同样的失败刷写（同镜像、同分区），`usage_logs.details_json` 中 `flash system` 那行**必须出现 `FAILED` 或 `fastboot: error`**。
3. 本地失败弹窗的「失败详情」同样能看到尾部错误行。
4. 随改动重编 exe 并重新部署（本轮只改客户端，服务端无需动）。

## 7. 本次 run v1:440 的处置

- 尾部内容**不可恢复**（证据见表）。
- 若现在就要那个真实错误：以同样镜像重跑一次 `system` 刷写，或手动执行
  `fastboot -s 400E6T00Y800000 flash system <镜像>` 直接看控制台；两者都需要设备在位（445 已报 `no link`）。
- 附带纠错：本次「三次分别卡在 `system` / `product` / `system`」说明失败点不固定在同一分区，
  加上速率 9.5 → 34 → 33 MB/s 的大幅波动与收尾的 `no link`，**更可能是 USB 链路问题**；
  拿到尾部错误行后即可判定是 `Write to device failed`（链路）还是 `remote: '…'`（设备侧策略）。
