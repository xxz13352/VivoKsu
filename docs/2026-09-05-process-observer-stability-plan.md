# Process observer loss 稳定性收口计划

## 问题与假设

`observer_failure_reports_loss_without_stopping_pipe_drain` 的 observer 只主动拒绝
首个 stdout callback，但端到端 fixture 同时快速发送多块输出。生产 dispatcher
使用容量 64 的有界队列和 2 ms 非阻塞发送预算；高并发调度下除了必然的 callback
failure，还可能产生合法的 queue-overflow loss。因此测试断言 loss **恰好为 1**
可能把两种不同来源的 loss 混为一谈。

先以隔离循环和多测试线程并发循环记录失败次数与实际 loss 数量；再判断是否存在
生产计数重复。如果同一 observation 被重复记录才修生产逻辑；若额外 loss 对应不同
output sequence，则修正测试边界。

## 最小修复

只修改 `nwflash-windows/src/process.rs` 内专属测试（除非复现证明生产计数重复）：

1. 增加确定性的 dispatcher 级测试，只投递一个 stdout observation，验证 observer
   callback failure 精确记录一次且定位到 stdout sequence 0；
2. 端到端进程测试继续验证命令完成、完整 pipe drain、Finished 可观测，并将 loss
   断言改为：非空、有界、包含首个 stdout callback loss；允许同时报告不同 sequence
   的真实队列 overflow；
3. 不增加 sleep、不忽略测试、不放宽 `PROCESS_OBSERVER_MAX_LOSSES`，不改变生产队列、
   重试、pipe drain 或进程完成语义。

## 修复前复现与判定

- 隔离目标测试连续 50 次：0 次失败；8 路并发运行目标测试 64 次：0 次失败。
- 4 路同时运行完整 lib suite、每个 suite 使用 8 个测试线程时，首轮 16 次中有
  8 个 suite 因调度/系统资源压力失败；已捕获目标断言 `loss=5` 与 `loss=4`。
- 加入临时诊断后以相同方式运行 12 次，目标测试失败 3 次：第 3/5/8 次分别为
  51、18、3 个 loss。不存在可记录的随机 seed；触发因素是线程/进程调度竞争。
- 完整 identity 显示每个 loss 都是不同的 stdout sequence，并始终包含 sequence 0：
  sequence 0 是 observer 主动拒绝；其余连续或离散 sequence 是容量 64 的队列在
  2 ms 预算内无法入队时按设计报告的 overflow。没有发现同一 identity 重复计数。

结论：生产 loss 计数符合有界、逐 observation 可观测的既有语义；不修改生产
dispatcher。将“callback 失败精确一次”移入单 observation 的确定性测试；端到端
测试验证所有 loss identity 唯一且有界，并保留首个拒绝、pipe drain 和完成断言。

## 验收

- 修复前后记录目标测试隔离/并发复现次数及 loss 数量；
- 目标测试循环 50 次；
- `cargo test -p nwflash-windows --lib -- --test-threads=1`；
- `cargo test --workspace` 或等价相关高并发套件；
- `cargo clippy -p nwflash-windows --all-targets -- -D warnings`；
- `cargo check --workspace --all-targets`；
- 对目标文件执行 rustfmt check，并运行 `git diff --check`。

## 修复后验收结果

- 确定性 callback-loss 测试通过；最终端到端目标测试连续 50 次为 50/50，
  8 路并发 64 次为 64/64。
- `cargo test -p nwflash-windows -- --test-threads=1` 通过：lib 66、driver 2、
  driver_installer 11，0 failed；同一全包以 8 测试线程运行也全部通过。
- `cargo test --workspace` 退出码 0，所有 workspace target 与 doc tests 通过；
  3 个既有嵌套 Cargo build probe 保持 ignored。
- `cargo clippy -p nwflash-windows --all-targets -- -D warnings` 与
  `cargo clippy --workspace --all-targets -- -D warnings` 均通过。workspace Clippy
  首次曾被另一 owner 正在写入的未跟踪 `file_manager_shell_contract.rs` 格式字符串
  编译错误阻断；该文件稳定后原命令重跑通过，本任务未修改该文件。
- `cargo check --workspace --all-targets`、目标 `rustfmt --check` 和
  `git diff --check` 均通过。

## 回滚与风险

回滚仅撤销本计划与 `process.rs` 专属测试改动。若复现显示相同 loss identity 被重复
计数，则停止测试收口并改为生产状态去重；若只出现不同 sequence，则保留有界且逐项
可观测的现有生产语义。
