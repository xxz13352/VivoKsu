# 文件传输 P1-03：shell 合同与 native E2E 发布前验证计划

任务编号：FT-P1-03  
状态：**仅计划，等待实现令**  
计划基线：`f6b7dd7`（已合入 FT-P1-02 及 `docs/2026-09-04-file-transfer-post-p1-validation.md`）  
日期：2026-09-04（Asia/Shanghai）

本卡承接 post-P1 验收报告，只定义验证与测试设计，不在本轮修改源码、测试、配置、分支/ref、部署或生成物。实现令下发前不得把下面的命令、fixture 或 feature-gated harness 落地。

## 1. 目标与边界

本卡只处理 post-P1 报告中的两个发布前 P1 缺口：

1. **Android/toybox shell 合同**：验证 `adb shell -T sh -c`、严格 quoting、`mv -n` 不覆盖提交、`rm -f` 后置确认，以及 `[ -e ]/[ -L ]` 对普通文件和 broken symlink 的判断。
2. **dedicated native WDIO E2E**：让 WebView 中的真实 `invoke` 穿过 Tauri serde、文件命令、coordinator 和注入式 executor；覆盖成功、失败、取消、冲突和重试，而不是在 WebView 里直接 mock `files_*` 命令。

本卡明确不做：

- 不连接真实设备作为默认 CI 前置；设备矩阵是可选的发布验收门，缺少设备时必须标成“未验证”，不能把 host fixture 当成真机证明。
- 不做哈希校验、跨步骤 serial/设备绑定、断点续传或文件页字节进度。
- 不修复 process observer flaky（32/33）或远端目录竞态；两项只作为独立 P2 backlog 记录，不在本卡偷偷改实现或断言。
- 不触碰 Safe Flash 任何文件、Web API、认证协议、部署/发布配置、签名材料或现有分支/ref。

## 2. 已有证据与必须保持的事实

post-P1 报告已证明：

- FileManager UI mock：16/16；连同 PageFactory：19/19；上传、下载、安装、停止入口、设备断开和安全错误文案均有 mock 证据。
- Tauri `files::tests`：19/19；application `file_manager` 8/8、`file_transfer` 4/4；coordinator 31/31。
- P1-02 事务顺序：upload 使用 UUID 远端 partial → push → no-replace promote；download 使用 sibling partial → pull → sync/no-follow → local no-replace promote；cleanup 未确认不会成功返回。
- `FileManagerPage` 的 e2e build 当前通过 `src/test/tauri-core.wdio.ts` 的 direct mock registry；若文件命令也注册 direct mock，调用会在 WebView 侧短路，不能算 native backend E2E。
- `AppState::try_new()` 的生产 coordinator 带本地保护与远端 permission gate；前端 E2E 的 auth/session mock 不会自动改变 Rust 侧 capability。native harness 必须显式解决 admission fixture，否则应把结果标为“只测 UI mock”。

## 3. 工作流与决策门

实现令下发后按以下顺序执行：

```text
冻结基线/环境检查
  ├─ A. shell-contract host fixture（无设备也可执行）
  ├─ A'. opt-in Android/toybox device matrix（有设备时）
  └─ B. dedicated native WDIO + e2e-only executor/gate seam
          ↓
命令/状态账本与失败分类
          ↓
发布门：不支持安全 no-replace 语义时 fail-closed，不回退覆盖式 mv
```

每个门都要输出机器可读的 pass/fail/blocked 状态。环境缺失（没有 POSIX shell、WebView2、WDIO 依赖、dedicated binary 或真机）不能被静默跳过，也不能改成只跑 direct mock 后宣称通过。

## 4. A：shell-contract 验证设计

### 4.1 两层合同，不混淆结论

需要同时保留两层：

| 层 | 目的 | 能证明什么 | 不能证明什么 |
| --- | --- | --- | --- |
| 可执行 host fixture | 在没有设备时运行生成的脚本、验证退出码/文件状态/quoting | builder 产出的脚本在选定 POSIX shell 下的 exact-target、no-replace、cleanup 后置逻辑 | Android toybox 的具体版本行为、ADB shell 参数拼接差异 |
| opt-in Android/toybox matrix | 在真实目标 shell 上运行同一合同 | 目标设备的 `sh`/toybox 支持和实际 `adb shell` 参数传递 | 未列入矩阵的设备、未来厂商定制 shell |

host fixture 的结果标题必须写“host shell contract”，不能写“Android compatibility”；只有设备矩阵通过后才能写 Android/toybox compatibility。

### 4.2 Host fixture 的执行方式

建议新增一个只服务测试的 shell-contract runner（实现时可放在 `nwflash-application`/`nwflash-tauri` 测试支持目录或独立 e2e fixture），要求：

1. 从 `FileManagerService` 生成真实 `CommandSpec`，提取 `shell -T sh -c` 的 script 和参数，不复制一份手写模板作为被测对象。
2. 在临时、唯一的 POSIX sandbox 中执行；远端测试路径使用 `/tmp/nwflash-ft-contract-<uuid>/...` 这类绝对路径，避免把生成脚本改写成相对路径，也不触碰主机根目录。Windows runner 需要显式选择 WSL、Git-for-Windows `sh` 或 CI 的 POSIX shell；找不到 shell 时返回 blocked/fail，不静默跳过。
3. sandbox 只包含本次 fixture 创建的 regular file、directory、symlink/broken symlink 和 sentinel bytes。清理只能针对该 UUID sandbox，禁止 wildcard 清理主机临时目录或用户目录。
4. 将每次场景的退出码、`source_exists`、`destination_kind`、`temp_exists`、`temp_is_symlink` 等布尔/枚举结果写入测试账本；不把完整本地路径或原始 shell 输出写入公共日志。
5. host shell 若与 toybox 的选项行为不同，场景结果要明确标记为“host-only”；不能用 host 通过覆盖 device blocked。

若选择用容器/WSL 固定 shell，必须把镜像/发行版版本和启动命令写进实现计划，并验证其 `mv`/`rm` 不是发行版 alias。不得下载未校验的任意 binary 作为“toybox 替身”。

### 4.3 Host 合同场景

对 upload promote script 至少执行：

1. temp 为 regular file、destination 不存在：退出 0，destination 内容完整，temp 消失。
2. destination 为 regular file：退出 73（或固定 conflict code），新内容不覆盖，temp 仍可由 cleanup 删除。
3. destination 为 directory：必须被识别为冲突/失败，不能把 temp 移入该目录后报告成功；若当前 shell 的 `mv -n` 会采用目录语义，记录为 P1 blocker，不能放宽断言。
4. destination 为 symlink 和 broken symlink：`[ -e ] || [ -L ]` 均应阻止 promote，链接本身和 temp 状态可观测且未被错误跟随。
5. temp 缺失、temp 为 directory、temp 为 symlink：promote 非成功，不能宣称 final 已提交。
6. temp/final 路径含空格、Unicode、单引号和以 `-` 开头的组件：脚本只使用统一 quote helper；账本确认没有未 quoting 的用户路径、wildcard 或裸覆盖式 `mv`。
7. 模拟检查与 `mv` 之间 destination 变成 directory/file 的竞态：至少确认不会把错误状态当成成功；无法在 host shell 原子地制造的竞态标为未覆盖。

对 cleanup script 至少执行：

1. regular temp 存在：`rm -f --` 后退出 0 且 `[ -e ] || [ -L ]` 均为 false。
2. temp 不存在：幂等退出 0，后置确认仍为 absent。
3. broken symlink：`rm -f` 删除链接，`[ -L ]` 后置检查失败条件不被漏掉。
4. temp 是 directory 或 rm 返回非零：退出非零，事务层必须把结果归为 cleanup-unconfirmed，不能吞掉。
5. 任意路径只出现一次精确 quoted operand；无 `*`、`rm -rf`、目录扫描或最终目标 operand。

### 4.4 Android/toybox opt-in 矩阵

有可用设备时，用环境变量显式开启，例如 `NWFLASH_FILE_TRANSFER_DEVICE_SERIAL` 和单独的测试 sandbox 根；未设置时不得尝试连接任意用户设备。前置检查：

- `adb devices` 中 serial 唯一且状态为可用；记录 API level、`ro.build.version.release`、`toybox --version` 的脱敏摘要。
- 在 `/data/local/tmp/nwflash-ft-p1-03-<uuid>` 创建本次专属 sandbox；不使用 `/sdcard` 用户目录，不读取或删除既有文件。
- 探测 `sh -c`、`mv -n`、`rm -f --`、`[ -e ]`、`[ -L ]` 的返回码/帮助信息。探测失败是 capability failure，不允许自动回退到可能覆盖的 `mv`。
- 用 fixture bytes 和唯一文件名执行 4.3 的 promote/cleanup 场景；验证只看本次 sandbox 的文件状态。测试结束用精确路径删除 sandbox，设备断开时把残留标为 cleanup-unconfirmed 并提示人工检查，不扩大删除范围。

建议覆盖 Android 10–15 的至少一个代表设备/镜像，并记录 toybox 版本；矩阵不足时报告“未覆盖的版本范围”。设备矩阵不是默认 CI 依赖，发布候选必须附上最近一次真实设备结果或明确的发布阻塞。

### 4.5 A 的通过标准

- host fixture 的所有 required cases 通过，且每个脚本由真实 builder 生成。
- 所有列入发布支持范围的 toybox 设备通过 no-replace、broken symlink 和 cleanup 后置检查。
- 任一 `mv -n`/`sh -c`/`rm --` 不支持、destination directory 误报成功、或 cleanup 无法确认时，状态为 failed/blocked；不得修改成覆盖式命令来“绿测试”。

## 5. B：dedicated native WDIO/WebView→Tauri E2E 设计

### 5.1 环境与 binary 门禁

实现前先确认：

```powershell
npm ci --prefix src/Nwflash.Desktop/e2e-tests
npm run pretest:native --prefix src/Nwflash.Desktop/e2e-tests
```

然后检查 `src/Nwflash.Desktop/src-tauri/target/e2e-native/debug/nwflash-desktop.exe` 存在且非空，并确认 `NWFLASH_ALLOW_EXTERNAL_E2E_BINARY` 未开启。WDIO 配置必须继续拒绝 production `target/debug` binary；外部 binary override 不得用于发布门。

专属 binary 使用现有 `e2e` feature 和 deterministic test verification key；不得把 WDIO plugin 或 e2e command 注册进生产 feature graph。若 WebView2/loader、Node 依赖或 dedicated build 缺失，WDIO 状态为 blocked，不改配置绕过。

### 5.2 后端 harness 的必要条件

当前 direct mock bridge 会在 JS 侧优先返回 `__wdio_mocks__`。为证明真实后端，实施时必须提供**只在 `cfg(feature = "e2e")` 下存在**的 executor/gate seam，满足：

1. `files_upload`、`files_download`、`files_install_apk` 的 WebView invoke 不注册 direct mock，最终进入 Tauri command serde。
2. 事务仍调用生产 `FileTransaction`/coordinator helper；注入只替换 process executor 和 deterministic permission/admission fixture，不复制另一套业务状态机。
3. harness 能按场景返回 `ProcessOutput`/`DomainError`，写入 pull temp bytes、制造 destination race、模拟 pending/cancel/cleanup failure，并记录 command order、timeout 和 terminal outcome。
4. e2e-only gate 能让测试获得一次明确的 allow/capability；其实现不得改变 production `CompositeOperationPermissionGate`。另用现有 coordinator Rust tests 覆盖真实 gate 的拒绝/释放语义。
5. 可通过 e2e-only reset/snapshot 接口读取脱敏账本。账本不得回传 bearer、原始本地完整路径、临时远端名或 adb stderr；只返回场景 id、阶段、参数形状、计数和安全错误分类。
6. e2e-only commands 不加入生产 `generate_handler!`；构建测试应断言 production feature 不含它们。Harness 状态按测试 session 隔离，结束时只清理自己创建的 local sandbox。

可选实现形态（实现令下发时二选一并在代码评审中冻结）：

- **executor provider**：让生产 wrapper 从 feature-gated provider 取 `SystemCancellableProcessExecutor` 或 `E2eRecordedFileExecutor`，公开 `files_*` 命令名不变；
- **e2e transaction command**：新增仅 e2e 的命令，直接调用同一个私有 `execute_file_transaction_with_executor`。若采用此形态，UI spec 必须同时证明真实 `files_*` payload/页面状态与 harness transaction 的映射，不能把 e2e command 通过命名伪装成生产命令。

无论采用哪种形态，都不能仅把现有 `FileManagerPage.test.tsx` 的 JS mock 搬进 WDIO 后宣称覆盖 Rust。

### 5.3 admission/busy/cancel fixture

native spec 的 bootstrap/session/auth 可以继续使用现有 direct mock，但文件操作开始前必须通过 e2e-only harness 明确建立测试 admission。场景至少包含：

- 一次 pending upload/download 持有 coordinator permit；第二次文件操作收到 `InProgress`，不 spawn 第二个 executor；
- 点击 FileManager “停止操作”只发一次真实 `operation_cancel`，后端 token 被观察到，事务先 cleanup 再进入 canceled/failed terminal；
- cancel 请求失败时 operation 仍保持 busy，直到真实事务终态；
- 每个成功/失败/取消场景结束后，harness 账本和 coordinator 都可再次取得 idle/permit。

如果 e2e-only gate 绕过了生产 session capability，报告必须注明“native command path + deterministic test gate”，并保留现有 Rust admission 测试作为安全门；不得把绕过 gate 的结果描述为线上授权链已验证。

### 5.4 Native spec 场景矩阵

新增专属 `file-transfer.e2e.ts`（名称可调整）至少覆盖：

| 场景 | UI 动作 | 真实后端断言 | UI 终态 |
| --- | --- | --- | --- |
| upload success | 选择本地源、点击上传 | push→UUID temp→no-replace promote；timeout 30m/30s；无 cleanup | 上传完成并刷新目录 |
| upload non-zero/spawn/timeout/output | 选择源、触发 fixture failure | 只 cleanup 精确 temp，不发 promote | 安全失败文案、按钮解锁 |
| upload cancel | 开始 pending upload、点击停止 | `operation_cancel`→终止观察→cleanup 10s | 操作已取消、可重试 |
| upload retry | 首次失败后再次选择/点击 | 第二次使用新 scenario/temp，第一次残留不影响 | 第二次成功 |
| download success | 选择远端文件、保存到指定路径 | sibling reservation→pull→sync/no-follow→local no-replace；temp 消失 | 下载完成 |
| existing/race destination | 目标已存在或 fixture 在 promote 前创建 | pull 不应截断旧目标；冲突后 cleanup | 冲突/失败，旧字节不变 |
| download cancel/failure | pending pull、停止或注入 read/timeout | temp 删除或 cleanup-unconfirmed；不产生 final | 取消/安全失败、可重试 |
| APK install | 选择 `.apk`、成功/失败/取消 | 单条 install，5m deadline；无 file promote/cleanup | 安装完成/失败/取消 |
| concurrent operation | 在 pending 期间重复点击 | 第二次被 coordinator 拒绝且不新增 command | 原操作终态后解锁 |

每个 spec 必须读取 native harness ledger，而不是只看页面文字；至少断言 serde 字段名、serial 由 Rust fixture 提供、无 browser-supplied serial、command order、timeout、cleanup count 和 terminal state。

### 5.5 Native E2E 命令与验收

建议命令（实现后执行，当前不执行）：

```powershell
npm run test:native --prefix src/Nwflash.Desktop/e2e-tests -- --spec ./specs/file-transfer.e2e.ts
npm exec --prefix src/Nwflash.Desktop/e2e-tests -- wdio run ./wdio.conf.ts --spec ./specs/file-transfer.e2e.ts
```

同时运行不依赖 native binary 的回归：

```powershell
cargo test -p nwflash-tauri --lib files::tests
cargo test -p nwflash-application --test file_manager --test file_transfer
cargo test -p nwflash-windows --lib file_ops::tests
cargo test -p nwflash-windows --lib process::tests
cargo check --features e2e
cargo clippy --workspace --all-targets -- -D warnings
npm exec --prefix src/Nwflash.Desktop -- vitest run src/pages/FileManagerPage.test.tsx src/components/PageFactory.test.tsx --run
```

native spec 缺 binary、loader、WebView2 或依赖时，保留完整失败日志和环境版本；不得改 `wdio.conf.ts` 放宽 binary 路径，也不得改 direct bridge 让测试绕过 Tauri。

## 6. 观测、报告与数据清理

每轮输出一份 machine-readable summary 和人类可读摘要，至少包含：

- commit/ref、操作系统、Node/Rust/WDIO/ADB/toybox 版本；
- host/device fixture 是否运行、哪些 case blocked；
- 每个阶段的 exit code、timeout、cleanup confirmed/unconfirmed、destination state；
- native E2E 是否真正穿过 Tauri（可由 harness ledger 的 command id 证明）；
- 不记录 bearer、session secret、原始 stderr、完整用户路径、设备用户目录内容或任意未脱敏 shell 输出。

host sandbox、native local temp、device `/data/local/tmp/nwflash-ft-p1-03-<uuid>` 均由 fixture 生成并精确清理。若进程崩溃或设备断开，报告残留 ownership/path 摘要，后续人工只检查该 UUID；不得执行 `rm -rf`、通配符或 workspace-wide 清理。

## 7. P2 项目隔离

以下项目只登记，不在 P1-03 实现：

1. `observer_failure_reports_loss_without_stopping_pipe_drain` 在 full/process suite 中偶发 loss 数 2/3（隔离重跑通过）；另开 process-observer stability card。
2. 远端 promote 在 destination 目录竞态下的 exact-target 语义；本卡 A 会把它作为 required contract 检查，修复实现另开 card，不借验证计划扩大范围。
3. `selectedRemote` 刷新后陈旧选择、无显式 retry 控件、字节级进度；继续由 UI/P2 卡负责。

## 8. 环境阻塞与发布判定

| 阻塞 | 判定 | 处理 |
| --- | --- | --- |
| 无 POSIX shell/WSL/Git Bash | host contract blocked | 在固定 CI image 执行；不把未执行当通过 |
| 无 Android 设备或 toybox 版本未列入矩阵 | device compatibility unverified | 发布报告标明范围；不得声称全设备支持 |
| `mv -n`/`rm --`/`sh -c` 不支持 | P1 fail/blocked | 选择安全等价 primitive 或保持 fail-closed；禁止裸 mv 回退 |
| e2e-tests 依赖缺失 | native E2E blocked | `npm ci` 后重试；保留 lockfile/Node 版本信息 |
| dedicated e2e binary/WebView2 loader 缺失 | native E2E blocked | 修复环境/构建门，不改 binary allowlist |
| symlink/reparse 权限不足 | symlink case unverified | 在具备权限的 CI runner 补跑；不跳过后宣称全绿 |
| e2e harness 无法提供 deterministic admission | native backend E2E invalid | 先补 feature-gated gate/executor seam，再执行 spec |

发布候选只有在 host required cases、native WDIO required cases 和目标设备矩阵（若产品声明支持该范围）都给出明确结果后，才能标记 P1-03 complete。任何安全语义不确定都保持 failed/blocked。

## 9. 预计实现范围与回滚

实现令下发后可能涉及：

- `nwflash-tauri` 文件事务的 feature-gated executor/gate harness、仅 e2e 的 ledger/命令注册；
- `nwflash-application` 文件命令 builder 的 shell-contract 测试支持（只补测试 seam，不改变 production no-replace 语义）；
- `nwflash-windows`/application 的专属测试 fixture（若需要执行 host shell contract）；
- `src/Nwflash.Desktop/e2e-tests/specs/file-transfer.e2e.ts`、其锁定依赖/运行脚本（只有确有必要才改）；
- 仅用于测试的 fixture/报告文件。

以上是未来实现范围，不代表本轮已修改。实现必须保持 e2e feature 与 production graph 隔离，并先由 owner 精确列出文件归属。

回滚策略：

1. 停止 native/device fixture，精确删除其 UUID sandbox；
2. 回滚只涉及本卡新增的 feature-gated harness、测试和 fixture 文件/变更，不使用 `reset`、`clean`、stash 或覆盖其他会话文件；
3. 不回滚或修改 Safe Flash、Web API、D1/部署、签名或生产用户数据；
4. 若设备 cleanup 无法确认，保留设备 sandbox 标识供人工检查，不以回滚命令扩大删除范围。

## 10. 完成定义

实现令下发后，只有同时满足以下条件才可关闭 FT-P1-03：

1. host shell-contract required cases 全部通过，且报告明确其非真机属性；
2. 目标 Android/toybox 矩阵中所有声明支持的版本通过 exact-target/no-replace/cleanup 后置检查，未知能力 fail-closed；
3. dedicated native WDIO 文件操作 spec 真实穿过 WebView → Tauri serde → coordinator → injected executor，至少覆盖 success/failure/cancel/retry；
4. production build 不包含 e2e harness/WDIO command，现有 P1-02、coordinator、process 回归保持通过；
5. 所有 blocked/flaky/P2 项列出 owner、下一张卡和复现命令；
6. 未触碰 Safe Flash、Web API、哈希/serial 绑定、部署/ref，未提交或推送未经授权的变更。

本文件完成后等待实现令；不得据此自动开始改源码或安装依赖。
