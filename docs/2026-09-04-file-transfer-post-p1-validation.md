# FT-POST-VALIDATION：文件传输真实步骤 mock 复验

日期：2026-09-04（Asia/Shanghai）  
验证起点：`HEAD=8f759ef`（`fix(file-transfer): use atomic temporary destinations`）。验证期间共享分支被其他会话推进到 `2bed8f7`；本报告结论针对起点提交中的 P1-02 代码，未改动后续会话文件。  
范围：FT-P1-02 文件上传、下载、APK 安装及其 UI/IPC/coordinator 收尾链路。

本轮只读验证，不连接真机，不修改源码、测试、配置、分支或部署文件；唯一新增文件是本报告。按任务要求，哈希校验、跨步骤 serial/设备绑定和真实设备行为不纳入本轮结论。

## 结论摘要

现有注入式 executor、Rust 事务测试和 FileManager UI mock 能完整证明“选择 → IPC 参数 → operation admission → 临时目标 → 传输结果 → promote/cleanup → UI 终态”的预期顺序。没有发现 P0 数据破坏路径；已有目标在 mock 竞态和失败场景中保持不变，cleanup 未确认不会返回成功。

仍有三个发布前验证缺口：Android toybox 的 `mv -n`/`sh -c`/`rm --` 兼容性未在设备上确认；native WDIO 环境未安装，无法把 UI mock 贯通到真实 Tauri binary；Windows 路径检查与最终 MoveFileExW 之间仍存在极窄 TOCTOU。详见“优先级筛选”。

## 验证环境与限制

- 所有 ADB 行为均由 `RecordedFileExecutor` 记录 `ProcessCommand` 并按队列返回 `ProcessOutput`/`DomainError`；没有启动 adb，也没有连接设备。
- pull mock 可写入临时文件、制造目标竞态或删除临时文件；因此能验证事务状态机，但不能证明设备 shell 的实际语义。
- 当前主机为 Windows。file-ops 的 broken-symlink fixture 因进程没有创建符号链接权限而跳过；普通文件、目录目标和 no-replace 路径均已执行。
- 工作区原有其他会话改动（例如 `.claude/launch.json` 删除、历史 docs/artifacts）保持不变，本轮未触碰。

## 按用户动作顺序的复验

### 1. 文件选择：本地源、远端条目与保存目标

`FileManagerPage` 的上传和 APK 安装使用单选 `open({ multiple: false, directory: false })`；下载使用 `save({ defaultPath: entry.name })`。对话框返回非字符串时直接结束，不发 IPC。远端目录由 `files_list` 返回的 `name/full_path/is_directory/size_bytes` 驱动，目录点击进入、文件点击选择。

证据：

- `npm exec -- vitest run src/pages/FileManagerPage.test.tsx --run`：1 个文件、16/16 通过。
- 覆盖精确保存路径、上传源路径、APK 路径、目录进入/返回、删除确认、设备断开和取消入口。
- `npm exec -- vitest run src/pages/FileManagerPage.test.tsx src/components/PageFactory.test.tsx --run`：2 个文件、19/19 通过；Fastboot/断开状态下文件按钮禁用。

结果：选择层没有把本地完整路径写入页面日志；取消选择不会产生文件操作调用。缺口是“对话框取消无 IPC”目前主要由代码分支保证，未有单独断言。

### 2. IPC payload 与当前设备 serial

前端只提交：

```text
files_upload      { sourcePath, remoteDirectory }
files_download    { remotePath, destinationPath }
files_install_apk { apkPath }
files_list        { remoteDirectory }
files_delete      { remotePath }
```

前端不接受或提交 serial。Tauri 命令在 Rust 侧通过 `DeviceRuntime::active_adb_serial()` 读取当前 `AdbConnected` 快照，计划测试使用 `RF8T123` 验证 `adb -s RF8T123 ...`。命令已在 `lib.rs` 注册，旧的浏览器 authority transfer command 不再注册。

结果：当前快照 serial 到 ADB 参数的静态/计划级链路通过。按任务明确排除跨步骤 serial 绑定，因此“等待 admission 期间设备换 serial”不作为本轮通过或失败依据。

### 3. OperationCoordinator admission、busy 与 cancel

`files_upload`、`files_download` 和 `files_install_apk` 都进入同一个 `OperationCoordinator::run_async`，事务 closure 在 permit 内 `spawn_blocking` 执行；开始时报告 stage，只有事务真正成功才报告 `1.0`。`operation_cancel` 调用 coordinator 当前 cancellation token，事务 cleanup 使用独立 cleanup deadline，不因原 token 已取消而跳过。

证据：

- `cargo test -p nwflash-application --test operation_coordinator -- --nocapture`：31/31 通过，覆盖并发拒绝、取消、permission deny、future drop 和 permit 释放。
- Tauri `transactional_wrapper_releases_coordinator_after_failure`：失败收尾后再次 `try_acquire_idle()` 成功。
- UI mock 覆盖传输中停止、重复点击只发一次 `operation_cancel`、取消请求失败时保持 busy、设备断开后不再发送取消。

结果：admission/busy/cancel 的状态机顺序符合预期。文件页没有独立的 retry 按钮，但失败的 `finally` 会解锁操作按钮，用户可以重新选择并重试。

### 4. ADB 参数、spawn、输出、退出码与 deadline

事务 mock 记录的命令形态和 deadline 如下：

| 阶段 | 参数形态 | deadline |
| --- | --- | --- |
| upload push | `adb -s <serial> push <local-source> <remote-temp>` | 30 分钟 |
| upload promote | `adb -s <serial> shell -T sh -c <quoted-script>` | 30 秒 |
| download pull | `adb -s <serial> pull <remote-source> <sibling-temp>` | 30 分钟 |
| remote cleanup | `adb -s <serial> shell -T sh -c <rm-and-postcheck-script>` | 10 秒 |
| APK install | `adb -s <serial> install -r <apk>` | 5 分钟 |

`CancellableProcessExecutor::run_with_timeout` 保留旧 `run` 实现的默认兼容层，生产 executor 调用已有的 `run_command_with_cancel`。Windows process runner 并行排空 stdout/stderr，单流保留上限为 8 MiB，取消/超时请求终止进程树并在约 2 秒窗口内 reap。

证据：

- `cargo test -p nwflash-windows --lib -- --nocapture`：一次完整运行 65/65 通过。
- `cargo test -p nwflash-windows --lib process::tests -- --nocapture`：一次运行 32/33，唯一失败为既有 `observer_failure_reports_loss_without_stopping_pipe_drain`（实际 loss 数 2/3，断言期望 1）；隔离重跑该测试 1/1 通过，属于观察队列时序 flaky，不是文件事务失败。
- 文件事务测试逐个记录 `ProcessCommand` 顺序、timeout、非零退出、spawn/timeout/output/read/cancel 错误。

### 5. Upload：远端 temp → no-replace promote → cleanup

成功路径由 `upload_transaction_pushes_to_owned_temp_then_promotes_without_cleanup` 验证：

1. 在授权后的 closure 中生成同一远端目录下的 `.nwflash-upload-<UUID>.partial`；
2. push 只写 temp；
3. promote 脚本先检查目标 `[ -e ] || [ -L ]`，再执行 `mv -n --`；
4. promote 后检查 temp 已消失、目标存在；成功时不再发 cleanup。

失败矩阵由同一 `RecordedFileExecutor` 覆盖：

- push 非零、spawn、timeout、输出超限、读取错误、取消：只发 push + 精确 temp cleanup，不发 promote；
- promote 冲突/非零：cleanup temp，最终目标不被覆盖；
- cleanup 返回错误：返回“清理未确认”，绝不 `Ok(())`；即使 primary 是取消，cleanup 未确认也被归类为失败而非伪造 canceled；
- 远端 cleanup 使用 `rm -f -- '<exact-temp>'` 后检查 `[ -e ] || [ -L ]`，没有 wildcard、`rm -rf` 或目录删除。

应用层 `transactional_builders_keep_explicit_temp_paths_and_quote_remote_promote` 进一步验证了空格、单引号、`--`、`-T` 和 no-replace 脚本 quoting。

### 6. Download：sibling reservation → 写入 → sync/no-follow → local promote

成功路径由 `download_success_promotes_sibling_temp_and_preserves_complete_bytes` 验证：

1. 最终目标先用 `symlink_metadata` 拒绝已有文件、目录、链接；
2. 在目标同目录以 `create_new` 预留 `. <name>.nwflash-download-<UUID>.partial`；
3. pull 写入 temp；
4. 以 no-follow regular-file handle 打开并同步，随后再次验证普通文件；
5. Windows 使用 `MoveFileExW`（不带 `MOVEFILE_REPLACE_EXISTING`），其他平台使用 hard-link + remove 的 no-replace helper；
6. promote 后验证 destination 为普通文件且 temp 消失。

覆盖场景：

- 已有目标：pull 前拒绝，旧字节保持不变且没有 ADB 命令；
- pull 非零、spawn、timeout、输出/读取错误、取消：删除 temp，不产生 final；
- pull 期间制造目标竞态：no-replace 失败，竞态目标字节保持不变，temp 清理；
- exit 0 但 temp 被模拟删除：后置检查失败并清理；
- 缺失父目录：reservation 前失败，不 spawn。

本地 helper `ensure_safe_directory` 检查父目录 ancestry，`create_exclusive_regular_file`/`open_regular_file_no_follow` 在 Windows 使用 `FILE_FLAG_OPEN_REPARSE_POINT`；因此常规 reparse/symlink 路径会 fail-closed。

### 7. 取消、断开、失败、冲突、cleanup 未确认

| 事件 | mock 观察到的结果 |
| --- | --- |
| 开始前取消 | 不创建 local temp、不生成 remote temp 命令、不 spawn |
| 传输中取消 | 事务返回 UserCancelled，随后仍发独立 cleanup；UI 显示“操作已取消”并解锁 |
| 设备断开/启动失败 | executor 错误被转换为阶段安全错误；cleanup 失败时明确“清理未确认” |
| 非零退出 | 不 promote，保留已有目标，清理本次 temp |
| timeout/output/read 错误 | process runner 终止/reap；文件事务不把错误当成功 |
| 目标预先存在 | 下载无 ADB；远端 promote 返回冲突，不覆盖目标 |
| promote 期间目标出现 | 本地 helper/远端脚本 fail-closed，旧目标保持 |
| cleanup 未确认 | 返回 ExternalTool 失败态，不伪报成功或已取消 |

真实设备断开和真实进程 spawn 没有在文件事务 closure 内发生；这里只验证注入 seam 与通用 process runner 的组合契约。

### 8. APK install、UI 终态与重试

APK install 只执行一条 `adb install -r <apk>`，使用 5 分钟 deadline 和 cancellation；成功/非零/spawn/timeout/cancel 测试都确认 APK 源文件不变、没有 upload/download promote 或 cleanup 命令。包管理器私有 staging 不由客户端声称拥有或清理。

FileManager UI 在成功时显示“下载已完成/上传已完成/APK 安装已完成”，上传成功后重新 `files_list`；失败显示安全错误文案，取消显示“操作已取消”，`finally` 后按钮恢复可用。当前没有自动 retry 按钮，重试依赖用户再次点击动作；没有独立回归测试证明“失败后第二次点击”完整走通。

## 命令与构建门禁

| 命令 | 结果 |
| --- | --- |
| `npm exec -- vitest run src/pages/FileManagerPage.test.tsx --run` | 16/16 通过 |
| `npm exec -- vitest run src/pages/FileManagerPage.test.tsx src/components/PageFactory.test.tsx --run` | 19/19 通过 |
| `cargo test -p nwflash-tauri --lib files::tests -- --nocapture` | 19/19 通过 |
| `cargo test -p nwflash-application --test file_manager --test file_transfer -- --nocapture` | 8/8 + 4/4 通过 |
| `cargo test -p nwflash-application --test operation_coordinator -- --nocapture` | 31/31 通过 |
| `cargo test -p nwflash-windows --lib file_ops::tests -- --nocapture` | 7/7 通过（symlink fixture 因权限跳过） |
| `cargo check -p nwflash-tauri -p nwflash-application -p nwflash-windows` | 通过 |
| `cargo clippy -p nwflash-tauri -p nwflash-application -p nwflash-windows --all-targets -- -D warnings` | 通过 |
| `cargo test --workspace`（本轮最新运行） | 除 `nwflash-windows` observer-loss flaky 外，其余通过；最终 64/65 windows tests，命令 exit non-zero |
| 任务文件 `rustfmt --check` | 通过 |
| `git diff --check` | 本任务验证时通过；随后其他会话的新文档改动引入 trailing whitespace，当前工作区检查命中该无关文件 |

此前相同代码内容的 workspace 运行曾全绿；本轮最新 workspace 失败与独立 process filter 的同一个 observer-loss 时序断言一致，隔离重跑通过。共享分支随后继续前进，当前状态中的 Cloudflare/API 文档和其他 docs 改动不属于本轮。

## 优先级筛选

### P0

无。mock 场景未发现覆盖已有目标、把不完整文件提交为成功或 cleanup 失败后返回成功的路径。

### P1（建议发布前处理/单独验收）

1. **真实 Android shell 兼容性尚未证明。** `mv -n`、`sh -c`、`rm -f --` 和 `[ -L ]` 是安全策略的关键；不支持时当前会 fail-closed，主要风险是上传功能在目标设备上全部失败。建议下一卡建立 toybox 版本/设备矩阵，验证成功、冲突、broken symlink 和 cleanup 后置检查。
2. **native WDIO 链路未执行。** `src/Nwflash.Desktop/e2e-tests/node_modules` 不存在，专用 `target/e2e-native/debug/nwflash-desktop.exe` 也不存在；现有 E2E 只有 list/delete 的 mock 场景。因而尚无真实 WebView → Tauri serde → coordinator → executor 的文件操作证据。建议下一卡安装锁定依赖、构建 dedicated e2e binary，并加入 upload/download/install 的成功/失败/取消/重试用例。
3. **远端 promote 的目录竞态需明确 exact-target 语义。** 当前脚本用 `mv -n -- temp destination`，未使用 `-T` 或等价的“destination 必须是文件路径”原语；若目标在初始检查后变成目录，某些 `mv` 实现可能把 temp 放进目录。后置检查目前只要求 destination 存在，未要求普通文件。建议下一卡在设备矩阵确认并采用支持度可验证的 `mv -nT`/等价原语，任何不支持都保持 fail-closed。

### P2（可排入后续质量卡）

1. **Windows reparse TOCTOU。** ancestry/no-follow handle 检查已覆盖常规路径，但检查与 `MoveFileExW` 之间仍有极窄竞态；若要消除需基于已打开目录句柄的 rename API。
2. **远端 UUID temp 没有独占预留。** UUID 碰撞概率极低，当前 push 会直接写入该 temp；可在后续设计中加入设备端独占 claim，不能用 wildcard 扫描代替。
3. **process observer 测试不稳定。** `observer_failure_reports_loss_without_stopping_pipe_drain` 在并行/full suite 中偶发 loss 数 2/3，隔离重跑通过；建议稳定队列时序或改为只断言有界且包含首个 loss。
4. **文件页状态质量。** `selectedRemote` 在目录刷新/切换后没有自动清理，可能保留旧路径；且失败后没有显式 retry 回归测试。后端仍会做路径校验，暂未观察到数据破坏。
5. **进度可观测性。** 文件事务目前只有阶段和完成时 `1.0`，没有字节级进度；不影响本卡原子性，但长传输 UX 仍较弱。
6. **Windows symlink fixture 未覆盖。** 当前运行账户无法创建 file-ops broken symlink，需在具备权限的 CI/专用测试账户补跑。
7. **全仓格式门禁受其他会话文件影响。** 定向任务文件已格式化；`cargo fmt --all -- --check` 仍命中 operation coordinator、Safe Flash、device、firmware、mirror、crash uploader、driver 等历史未提交改动，本轮没有格式化它们。

## 建议下一张卡

建议建立 `FT-P1-03`：在受控 Android 设备矩阵上验证远端 shell 脚本的 no-replace/cleanup 语义，同时补齐 dedicated native WDIO 文件操作场景；并把远端 exact-target promote 和 process observer flaky test 作为同一验收门。哈希校验、跨步骤 serial 绑定和更细粒度字节进度继续单列，不在本报告中扩大范围。
