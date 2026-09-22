# VIVO 线刷：实时进度、日志时间戳与逐分区日志（2026-09-22）

承接 09-21 的受保护分区「假刷写」改造。本轮处理用户报告的 5 个可见问题，
全部已提交（`9106fdc`…`22b9653`）。

## 用户报告与结论

| 报告 | 根因 | 状态 |
|---|---|---|
| 刷写分区日志显示 `...` / `... OK` | 后端每个分区发两条 stage（下发前 `刷写分区[i/n] ...`、成功后原地改写 `... OK`），后缀会让日志看起来"会自我刷新" | 已修：每步只报一次 `刷写分区[i/n]`，成功/重试/跳过都不改写 |
| 客户端日志时间全是"第一个分区的时间" | 两条日志来源都用 `operationSnapshot.startedAt`——那是**整个操作**的起点、全程不变；服务端正确是因为它按各自落库时刻打点 | 已修：改用快照真正到达的时刻 |
| 日志不自动滚动 | 滚动宿主是 `.nw-operation-log-body`，代码滚动的是外层面板（`scrollHeight == clientHeight`，赋值无效） | 已修：操作真实容器 + 按用户滚动意图跟随 |
| payload 提取 / fastboot 刷写没有实时进度 | ① 整条 `fastboot flash` 是阻塞调用，只在分区边界报一次；② 进度条只认 `刷写分区[i/n]`，最耗时的下载/解包整段不显示 | 已修：解析 fastboot 实时输出 + 放开阶段门槛 |
| 假刷写进度条一直左右波动、看不到百分比 | 前端用 `fraction > 0` 当"有没有刻度"的判据，假刷写从 0% 起步被误判成"没刻度" | 已修：后端用 `has_scale` 明确表态 |

## fastboot 实时进度（本轮主要新增）

本项目所用 fastboot 带实时回调，逐行打印：

```
Sending 'system' (393216 KB)...
Sending sparse 'system' (65536 KB)...
system: 32768 KB/65536 KB
Writing 'system'...
Finished. Total time: 12.345s
```

新增 `FastbootProgressParser` 解析真实传输量，经 `ProcessOutputObserver` 挂在
真实执行器外面（仅 `system_executor`）。三个要点均有回归测试：

1. **大镜像被切成多个 sparse 块**，`Writing` 只代表**当前块**完成，不能一见
   它就记满整张镜像；按块累计，`Sending` 重置块内计数，`Writing` 补齐余量。
2. **`error` 分词要把 `_` 当词内字符**，否则合法分区名 `error_log` 会被误判
   成刷写失败（参考实现的 `\berror\b` 在此仍有边界问题）。
3. **累计量以本地镜像大小为上限**；失败后不再臆造进度。

fastboot 输出被重定向/静默时保留按耗时估算的兜底（95% 封顶，命令返回后补满），
估算只影响展示，不影响成功判定与取消语义。

## 顺带修掉的两个真缺陷

- **观测线程卡死**：`run_command_with_cancel_observed` 每次调用都新起观测线程
  并在返回前 join。无条件包装所有 flash 命令后，每个伪造 flash 的用例都卡在
  线程排空上——`nwflash-tauri` 测试从 1.9 秒变成 **8 分钟以上不返回**（实测确认
  并杀掉）。改为仅对 `system_executor` 包装。
- **误粘的右花括号**：删除重复测试时，被删块末尾的 `}` 与下一段文档注释粘到
  同一行（`}/// 反调试挂起：…`）。能通过解析纯属侥幸，已拆回。

## 进度显示的两条条

后端 `report_now_partition_task(name, progress, has_scale)` 用
`PartitionTaskState::Running`/`Waiting` 表达"有没有刻度"：

- 有刻度（真刷写与假刷写**都有**，假刷写的镜像照常解包落盘）→ 显示真实
  百分比，**包括刚起步的 0%**；
- 没刻度（后端也不知道分区多大）→ 前端走左右波动的不确定态，且**不设
  `aria-valuenow`**，百分比文字显示 `--`。

VIVO 线刷的 `刷写分区[i/n]` 走 `report_stage_without_log`：只更新界面状态行，
**不写本地操作日志**。文案本身只含 i/n、不含分区名。未接线的调用点（快捷刷写、
分区页）保持原行为，仍走 `report_partition_task` 并在日志里显示分区名。

## 证据

- Rust：`safe_flash` 45/45（含 8 个解析器单测 + 2 个分区进度测试）、
  `nwflash-domain` + `nwflash-application` 全绿、`nwflash-tauri` lib 335/335
- 前端：vitest 257 通过、`tsc --noEmit` 干净、`npm run build` 通过
- `cargo clippy`：`nwflash-application` / `nwflash-tauri` 无新增告警
- 其中 3 个新测试**写完当时即失败**，确认在测真东西：多 sparse 块首块跳满、
  `error_log` 误判、假刷写 0% 被当"没刻度"（旧的 `> 0` 判据下报
  `expected true to be false`）

## 未闭合

- **真机未验证**：假刷写百分比、fastboot 输出解析都只在 mock/夹具层面验过。
  解析依赖所用 fastboot 的实际打印格式，夹具是按参考项目推的。
- **全仓 `cargo fmt` 不干净**：HEAD 上实测 **91 处**既有偏差，横跨所有 crate
  （含本轮完全没碰的 `nwflash-windows`/`nwflash-protection`/
  `nwflash-infrastructure`）。按仓库既有约定（见
  `2026-09-04-file-transfer-post-p1-validation.md:182`、
  `superpowers/plans/2026-08-19-scrcpy-resource-provisioning.md:136`、
  `superpowers/plans/2026-08-20-https-firmware-extraction.md:84`：**只格式化本次
  改动的文件**）本轮未扩大范围。发布前若要求 fmt 门禁全绿，需单列一张卡。
- **`windowPermissions.test.ts` 3 条红灯**：既有问题（capability 期望过期 +
  CRLF 断行断言），已用 `git stash` 在未修改的树上对照确认与本轮无关，未修。
- **未跑**：native WDIO、全仓 fmt（见上）、protected 构建、真机矩阵。上述定向
  证据**不能**替代发布验收。
- 提交未推送。
