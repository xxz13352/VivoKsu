# Nwflash 管理员 Ops Console 与结构化完整日志设计

> 状态：**已确认**。用户已选择 A · Ops Console，批准管理员后台与日志架构，并授权持续推进。

## 一、目标

本设计同时解决两个不可分割的问题：

1. 完全重写 `web.nwflash.cc.cd` 管理员界面，删除现有横向页签式 UI，改为可持续扩展的 Ops Console。
2. 把当前“一次操作一行摘要”的 `usage_logs` 升级为可审计的完整追踪链路，使服务器能够按用户查看一次操作中的全部阶段、分区、命令、退出码、stdout、stderr、成功证据与失败原因。

核心验收结果：管理员可以沿着

`用户 → 操作 → 步骤/分区 → 成功详情或失败诊断 → 完整命令日志`

逐级查看真实服务器记录。成功和失败必须具有同等详细度，不能只对失败保留命令证据。

## 二、范围与所有权

### 2.1 本设计负责

- `cloudflare/web` 管理员后台的 UI、管理员 API 与 D1 查询。
- 客户端结构化追踪契约、执行器采集、持久重试队列与上传 API。
- D1 V2 日志表、迁移、旧记录兼容、保留策略与管理员审计。
- VIVO 线刷的逐分区、逐命令成功/失败记录，并把相同契约扩展到其他工具操作。

### 2.2 明确不负责

- 不修改 `cloudflare/user`。用户门户由独立任务负责。
- 用户门户只允许 `activity → operation → step` 的脱敏信息；原始命令、完整 stdout/stderr、完整设备序列号、路径、令牌与签名 URL 保持管理员专属。
- 不修改或还原当前工作树中属于其他会话的桌面端会话、安全或发布改动。
- 不在设计批准前修改生产 UI、Worker API、D1 或 Rust 代码。

## 三、已确认的产品决策

| 决策 | 选择 | 结果 |
| --- | --- | --- |
| 管理员布局 | A · Ops Console | 固定侧栏、顶部上下文、数据工作区、逐级详情 |
| 主导航 | 恰好六项 | 概览、版本策略、用户管理、在线会话、操作审计、ROM 查询 |
| 操作日志入口 | 先按用户归类 | 默认不加载所有命令噪音 |
| 日志层级 | 五级下钻 | 用户、操作、步骤、执行详情、命令日志 |
| 成功日志 | 与失败同等详细 | 命令、参数、输出、退出码、验证结果全部记录 |
| 管理员字段 | 完整运营字段 | IP、设备序列号、路径、URL、命令参数与工具输出不做展示打码 |
| 凭据边界 | 从源头不采集 | 密码、Bearer token、Cookie、私钥、签名密钥不进入追踪对象 |
| 技术路线 | 保留 Cloudflare Worker + 原生浏览器模块 | 不引入 React/Vue，不增加远程字体或远程脚本 |
| 旧日志 | 双读降级 | V1 摘要继续可见，但不伪造不存在的步骤或命令 |

## 四、Fate 三视角合议结论

### 4.1 道士：先定产品层级

管理员后台是运营与故障定位工具，不是数据卡片集合。全局应用壳、导航、状态语言、危险操作协议和页面状态必须先统一，再讨论单页装饰。

### 4.2 Seri：找到真正的卡点

卡点位于 `CancellableProcessExecutor::run` 与 `SafeFlashExecutionService::run_required` 共存的边界。该位置同时持有完整 `ProcessCommand` 和 `ProcessOutput`；离开后，参数、路径、stdout、stderr 与分区上下文被丢弃。日志采集必须在此边界发生，后台无法事后重建这些事实。

### 4.3 Cat：执行证据高于乐观反馈

前端不能根据模拟数据、toast 或单独的退出码 `0` 宣称成功。成功必须由服务端持久化的终态加上操作特定后置条件共同证明。危险操作必须经历确认、pending、服务端结果、权威状态刷新与可重试失败五个阶段。

## 五、管理员信息架构

### 5.1 应用壳

- 左侧 196px 固定导航；窄屏转换为带文字的紧凑顶部导航。
- 顶栏显示 `NWFLASH / ADMIN / 当前页面`、全局搜索与管理员菜单。
- 全局搜索覆盖用户、账号、run ID、event ID、分区、错误码、设备序列号与 URL。
- 生产健康状态位于侧栏底部，不占用主导航。
- 退出、改密和管理员维护放在账户菜单，不创建第七个菜单。

### 5.2 六个菜单

#### 概览

- API 用户数、当前在线、今日操作、今日失败。
- 24 小时操作量与失败趋势。
- 最近失败列表；每行直接进入对应 run。
- 所有数值来自服务端聚合接口，不在浏览器根据最近若干行估算。

#### 版本策略

- 当前版本、最低版本、受支持版本、今日 426 数量。
- 搜索、状态筛选、登记、编辑、启用与停用。
- 版本删除使用应用内确认对话框，明确影响客户端准入策略。

#### 用户管理

- 搜索账号、名称、备注与状态。
- 创建用户、查看详情、重置密码、轮换 token、启用、停用、封禁、解封、删除。
- token 仅在创建或轮换成功后显示一次；复制动作不进入通用日志正文。
- 删除、封禁、轮换 token 使用应用内确认对话框。

#### 在线会话

- 完整会话 ID、客户端版本、完整 IP、上线时间、最后心跳与在线时长。
- 每 10 秒刷新，但页面不可见、登出或请求未完成时暂停轮询。
- 强制下线显示目标会话、用户和原因；默认焦点落在“取消”。
- 提交后行状态变为“下线请求已发送”，直到服务端不再返回该会话才移除。

#### 操作审计

- 默认显示第 1 级用户汇总。
- 支持时间、状态、工具、用户、分区、错误码与全文搜索。
- 支持五级下钻、面包屑、深链接、浏览器前进/后退和焦点恢复。
- 支持导出当前筛选范围；导出动作写入 `admin_audit_log`。

#### ROM 查询

- 完整显示时间、用户、PD、版本、HTTP 状态、完整结果 URL 或失败原因。
- 支持用户、PD、版本、状态和 URL 搜索。
- ROM 查询 V2 记录可作为单步骤 operation 展示；旧 `access_logs` 仍保持原表视图。

## 六、五级日志交互

### 第 1 级：用户汇总

每个用户一行：显示名、账号、操作总数、失败数、最后操作和最后活动时间。选择用户后进入第 2 级。

### 第 2 级：精简操作日志

一次 operation 一行：开始时间、标题、工具、run ID、终态、耗时、客户端版本。此层不加载命令输出。

### 第 3 级：操作步骤与分区

按 sequence 显示全部事件：授权、阶段、分区探测、刷写、跳过、槽位切换、清除、重启与终态。状态闭集为：

- `RUNNING`
- `SUCCESS`
- `FAILED`
- `CANCELED`
- `SKIPPED`
- `UNKNOWN`

成功和失败步骤均有“查看详情”。失败后未执行的计划步骤显示 `SKIPPED`，不能从列表中消失。

### 第 4 级：步骤执行详情

成功步骤显示：

- 结果类别、阶段、分区、退出码、步骤序号。
- 完整返回内容。
- 操作特定成功后置条件。
- 关联命令数、持久化终态与耗时。

失败步骤额外显示：

- error class、error code、具体失败原因。
- 最后成功步骤、首个失败步骤、设备停止状态。
- 是否可安全重试、是否需要人工处理。
- 可执行处理建议。

### 第 5 级：完整命令日志

每条命令显示：

- 命令序号、完整程序路径、完整 argv、工作目录。
- 完整设备序列号、分区、源/目标路径和 URL。
- 精确开始与结束时间、耗时、退出码。
- 独立的 stdout 与 stderr 分块。
- 关联 run ID、event ID、验证结果与输出完整性状态。

空 stdout/stderr 明确显示 `(empty)`。长路径和输出只在自身代码区滚动，不产生页面级横向滚动。

## 七、浏览器交互与无障碍规范

### 7.1 路由和状态

管理员 UI 使用 URL 表达状态：

```text
/?view=audit
  &userId=42
  &runId=019d...
  &eventId=019d...:13
  &level=command
  &stream=stderr
```

- 刷新和直接打开链接恢复相同层级。
- 浏览器后退回到来源行，恢复筛选、分页、滚动位置与焦点。
- URL 不包含密码、token、Cookie、命令输出或其他凭据。

### 7.2 键盘与焦点

- 六菜单支持 Tab、Enter/Space、ArrowUp/ArrowDown、Home/End。
- 面包屑当前层使用 `aria-current="page"`，不是禁用伪链接。
- 下钻后焦点移动到新层级标题；返回后焦点恢复到来源行。
- 对话框捕获焦点，Escape 关闭，关闭后返回触发按钮。
- 所有交互目标至少 44×44 CSS px。

### 7.3 页面状态

每个数据页必须实现：

- 初始加载。
- 空结果。
- 部分数据或 trace 不完整。
- 数据陈旧。
- 未授权/会话过期。
- 请求失败和显式重试。
- mutation pending、成功与失败。

持久错误使用 `role="alert"`；普通完成通知使用独立、简短的 `role="status"`。整个日志树不得设为 live region。

### 7.4 响应式

- 320、360、768、1024 与宽桌面尺寸均不得出现 body 级横向滚动。
- 窄屏活动行转换为堆叠摘要，隐藏字段在详情页仍可查看。
- 搜索和筛选不能简单消失；可折叠到“筛选”面板。
- 核心正文至少 13px；代码与时间戳至少 12px。
- 遵守 `prefers-reduced-motion`，对比度满足 WCAG AA。

## 八、前端文件结构与交付方式

继续使用 Cloudflare Worker，不引入前端框架或远程资源。删除旧 UI 内容后按职责重建：

```text
cloudflare/web/src/admin/
├─ index.html              # UTF-8 文档壳、登录视图、应用挂载点
├─ styles.css              # tokens、shell、组件、响应式、状态样式
├─ app.js                  # 启动、认证恢复、页面生命周期
├─ api.js                  # fetch、错误分类、CSRF 头、会话失效
├─ router.js               # URL 状态、前进/后退、深链接
├─ components.js           # dialog、toast、status、pagination、focus helpers
└─ pages/
   ├─ overview.js
   ├─ versions.js
   ├─ users.js
   ├─ sessions.js
   ├─ audit.js
   └─ rom.js
```

`cloudflare/web/src/index.ts` 继续负责管理员鉴权和 API，并以同源、`no-store`、正确 MIME 类型提供上述静态模块。`wrangler.toml` Text rules 扩展到 HTML、CSS 与浏览器 JS。CSP 保持同源脚本，不加载 Google Fonts、CDN 或远程模块。

动态 API 内容使用 `textContent` 或统一 `escapeHtml`，禁止把 stdout、stderr、路径、URL 或用户名直接插入未转义 `innerHTML`。

## 九、V2 结构化追踪契约

### 9.1 追踪生命周期

```text
OperationCoordinator 在授权前生成 run_id
  → 记录 authorization event
  → 记录 stage / partition / skip event
  → 进程执行器记录 command started
  → 进程结束记录 command terminal + output chunks
  → 操作特定后置条件验证
  → 记录 terminal event 与 run outcome
  → 写入本地持久队列
  → POST /api/usage/traces/v2
  → D1 事务落库并返回逐项 ack
```

run ID 使用 UUIDv7。event ID 为独立 UUIDv7，`(run_id, sequence)` 额外唯一。run ID 在授权前生成，使准入拒绝、授权失败和取消也能形成完整终态。

### 9.2 Rust 类型

```rust
pub const TRACE_SCHEMA_VERSION: u16 = 2;

pub enum TraceOutcome {
    Running,
    Success,
    Failed,
    Canceled,
    Denied,
    Aborted,
    Unknown,
}

pub enum TraceEventKind {
    Authorization,
    Stage,
    Partition,
    Command,
    Skip,
    Verification,
    Terminal,
}

pub enum TraceEventStatus {
    Started,
    Success,
    Failed,
    Canceled,
    Skipped,
    Unknown,
}

pub struct OperationTraceRun {
    pub run_id: String,
    pub operation_kind: String,
    pub title: String,
    pub outcome: TraceOutcome,
    pub device_serial: Option<String>,
    pub source_paths: Vec<String>,
    pub source_urls: Vec<String>,
    pub client_version: String,
    pub started_at_ms: i64,
    pub ended_at_ms: Option<i64>,
    pub duration_ms: Option<i64>,
    pub error_class: Option<String>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub final_sequence: Option<u32>,
    pub trace_complete: bool,
    pub trace_loss_reason: Option<String>,
}

pub struct OperationTraceEvent {
    pub event_id: String,
    pub run_id: String,
    pub sequence: u32,
    pub kind: TraceEventKind,
    pub step_name: String,
    pub partition_name: Option<String>,
    pub status: TraceEventStatus,
    pub started_at_ms: i64,
    pub ended_at_ms: Option<i64>,
    pub duration_ms: Option<i64>,
    pub command: Option<TraceCommand>,
    pub exit_code: Option<i32>,
    pub stdout_chunks: u32,
    pub stderr_chunks: u32,
    pub verification: Option<String>,
    pub error_class: Option<String>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub credential_redactions: Vec<CredentialRedactionCount>,
}

pub struct TraceCommand {
    pub program: String,
    pub argv: Vec<String>,
    pub display_command: String,
    pub working_directory: Option<String>,
    pub paths: Vec<String>,
    pub urls: Vec<String>,
    pub serial: Option<String>,
}

pub struct TraceOutputChunk {
    pub chunk_id: String,
    pub event_id: String,
    pub stream: OutputStream,
    pub chunk_index: u32,
    pub text: String,
    pub byte_count: u32,
    pub sha256: String,
}
```

命令 environment、HTTP headers、登录请求体和 Cookie 不属于 `TraceCommand`，没有进入追踪模型的入口。

### 9.3 成功证据

退出码 `0` 不是统一成功条件。每类操作定义后置验证：

- fastboot flash：退出码 `0`，输出包含完成标记，且对应 partition event 持久化成功。
- reboot：命令成功发送；若流程要求重新连接，则必须记录后续设备状态。
- 文件传输：退出码 `0`，并记录已传字节数；能够校验时记录目标存在或大小。
- 下载：HTTP 成功、字节数与完整性验证结果。
- ROOT 修补：输出工件存在、非空且哈希完成。
- ROM 查询：HTTP 状态与解析后的业务结果。

验证结果写入 `OperationTraceEvent.verification`，前端只展示服务端保存的结果。

## 十、凭据与完整运营字段边界

### 10.1 管理员可见且必须保存

- 完整设备序列号与来源 IP。
- 完整程序路径、argv 与工作目录。
- 完整本地路径、远程路径、分区名与非凭据 URL。
- 完整、有序的 stdout/stderr 内容。
- run/event/sequence、时间、耗时、退出码、验证结果与错误原因。

### 10.2 永不采集

- 密码与密码字段。
- `Authorization` / Bearer token。
- Cookie / Set-Cookie / 管理员 session token。
- API key、OAuth secret、签名 secret 与私钥块。
- 命令 environment 中的 secret 值。

### 10.3 凭据防线

“不脱敏管理员运营字段”不等于把凭据存入 D1。客户端在创建 spool 记录前同步剔除凭据；Worker 入库前再次检查：

- Bearer/Cookie/password/token/api-key/secret/signature 模式。
- URL userinfo 和凭据型 query 参数。
- PEM/OpenSSH 私钥块。
- 当前运行会话注册的确切 secret 值。

命中凭据时只替换凭据本身，完整保留命令、路径、URL host/path、非凭据参数、序列号与工具上下文。记录 redaction 类型和数量，不记录 secret 哈希。无法安全解析的高风险字段替换为 `[CREDENTIAL_REMOVED:HIGH_RISK]`。

## 十一、本地持久队列

替换当前仅内存的 `UsageLogReporter` 队列，新增每用户、每 run 的磁盘 spool：

```text
AppData/Nwflash/trace-spool/v2/<run_id>/
├─ run.json
├─ events.jsonl
└─ output/
   └─ <event_id>.<stdout|stderr>.<chunk_index>.json
```

- 先写同目录临时文件，flush/sync 后原子替换。
- 只保存已经完成凭据检查的内容。
- 每个 output chunk 最大 32 KiB，保留 UTF-8 字符边界。
- `TraceOutputChunk.sha256` 计算已经完成凭据检查、实际写入 spool 与 D1 的文本，不计算原始未检查字节。
- 服务器逐项 ack；只删除已确认的 run/event/chunk。
- 上传失败不丢弃当前 chunk，也不丢弃尚未尝试的尾部。
- 进程启动重放；关闭时在超时预算内真实 flush，不能先设 stopped 再短路 flush。
- 未确认记录保留 7 天。IO、配额或缺块时设置 `trace_complete = false` 和 loss reason，不得宣称日志完整。

## 十二、上传 API

### 12.1 客户端端点

`POST /api/usage/traces/v2`

要求 bearer 客户端鉴权。请求限制：

- 最大 1 MiB。
- 最多 20 runs。
- 最多 100 events。
- 最多 200 output chunks。
- 单 chunk 最大 32 KiB。
- enum、UUID、时间、sequence、归属和长度严格校验。

响应逐项确认：

```json
{
  "ok": true,
  "accepted": {
    "runs": ["019d..."],
    "events": ["019d..."],
    "output_chunks": ["019d..."]
  },
  "rejected": []
}
```

同一用户重复 ID 返回成功幂等确认；属于另一用户的冲突返回 `409`。服务端从可信 `CF-Connecting-IP` 写入 source IP，不接受客户端 body 中的 IP。

### 12.2 管理员端点

- `GET /api/usage-logs/v2/users?from&to&status&q&limit&cursor`
- `GET /api/usage-logs/v2/runs?userId&kind&status&from&to&q&limit&cursor`
- `GET /api/usage-logs/v2/runs/{runId}`
- `GET /api/usage-logs/v2/runs/{runId}/events/{eventId}`
- `GET /api/usage-logs/v2/runs/{runId}/events/{eventId}/output?stream&afterChunk&limit`

全部端点要求管理员 session Cookie，设置 `Cache-Control: no-store`，使用 keyset cursor `(started_at_ms, run_id)`，不使用不断变慢的 offset 分页。

完整输出查看与导出分别写 `admin_audit_log`：

- `view_trace_output`
- `export_trace`

## 十三、D1 V2 数据结构

```sql
CREATE TABLE usage_operation_runs (
  run_id TEXT PRIMARY KEY,
  api_user_id INTEGER NOT NULL,
  api_user_name TEXT NOT NULL,
  schema_version INTEGER NOT NULL CHECK(schema_version = 2),
  operation_kind TEXT NOT NULL,
  title TEXT NOT NULL,
  outcome TEXT NOT NULL CHECK(outcome IN
    ('running','success','failed','canceled','denied','aborted','unknown')),
  device_serial TEXT,
  source_ip TEXT,
  source_paths_json TEXT NOT NULL DEFAULT '[]',
  source_urls_json TEXT NOT NULL DEFAULT '[]',
  client_version TEXT NOT NULL DEFAULT '',
  started_at_ms INTEGER NOT NULL,
  ended_at_ms INTEGER,
  duration_ms INTEGER,
  error_class TEXT,
  error_code TEXT,
  error_message TEXT,
  final_sequence INTEGER,
  trace_complete INTEGER NOT NULL DEFAULT 0 CHECK(trace_complete IN (0,1)),
  trace_loss_reason TEXT,
  created_at INTEGER NOT NULL DEFAULT (strftime('%s','now')),
  updated_at INTEGER NOT NULL DEFAULT (strftime('%s','now'))
);

CREATE TABLE usage_operation_events (
  event_id TEXT PRIMARY KEY,
  run_id TEXT NOT NULL,
  sequence INTEGER NOT NULL,
  event_kind TEXT NOT NULL,
  step_name TEXT NOT NULL,
  partition_name TEXT,
  status TEXT NOT NULL,
  started_at_ms INTEGER NOT NULL,
  ended_at_ms INTEGER,
  duration_ms INTEGER,
  command_program TEXT,
  command_argv_json TEXT,
  command_line TEXT,
  working_directory TEXT,
  paths_json TEXT NOT NULL DEFAULT '[]',
  urls_json TEXT NOT NULL DEFAULT '[]',
  serial TEXT,
  exit_code INTEGER,
  stdout_chunks INTEGER NOT NULL DEFAULT 0,
  stderr_chunks INTEGER NOT NULL DEFAULT 0,
  verification TEXT,
  error_class TEXT,
  error_code TEXT,
  error_message TEXT,
  credential_redactions_json TEXT NOT NULL DEFAULT '[]',
  created_at INTEGER NOT NULL DEFAULT (strftime('%s','now')),
  UNIQUE(run_id, sequence)
);

CREATE TABLE usage_output_chunks (
  chunk_id TEXT PRIMARY KEY,
  event_id TEXT NOT NULL,
  stream TEXT NOT NULL CHECK(stream IN ('stdout','stderr')),
  chunk_index INTEGER NOT NULL,
  text TEXT NOT NULL,
  byte_count INTEGER NOT NULL,
  sha256 TEXT NOT NULL,
  created_at INTEGER NOT NULL DEFAULT (strftime('%s','now')),
  UNIQUE(event_id, stream, chunk_index)
);

CREATE INDEX idx_trace_runs_time
  ON usage_operation_runs(started_at_ms DESC, run_id DESC);
CREATE INDEX idx_trace_runs_user_time
  ON usage_operation_runs(api_user_id, started_at_ms DESC, run_id DESC);
CREATE INDEX idx_trace_runs_kind_status_time
  ON usage_operation_runs(operation_kind, outcome, started_at_ms DESC);
CREATE INDEX idx_trace_events_run_seq
  ON usage_operation_events(run_id, sequence);
CREATE INDEX idx_trace_events_partition_status
  ON usage_operation_events(partition_name, status, started_at_ms DESC);
CREATE INDEX idx_trace_output_event_stream
  ON usage_output_chunks(event_id, stream, chunk_index);
```

## 十四、保留策略

- run 摘要：180 天。
- event/步骤元数据：90 天。
- command、完整路径、URL、IP、serial 与 stdout/stderr：30 天。
- 到期数据执行删除或字段清除，不用展示打码替代删除。
- 清理任务记录删除数量和截止时间，不把被删内容复制到 Worker console。

## 十五、迁移与旧客户端兼容

1. 新增幂等 D1 migration，不修改或删除 `usage_logs`。
2. 先部署 V2 表和 `/api/usage/traces/v2`。
3. V2 terminal run 在同一事务中投影摘要到旧 `usage_logs`，`event_key = run_id`。
4. 部署 V2 客户端；旧客户端继续使用 `/api/usage/logs`。
5. 管理员后台双读：V2 优先，V1 显示 `source_schema = 1`。
6. V1 行只提供第 2 级摘要；第 3 级显示“旧客户端未上传步骤数据”，不生成虚构步骤。
7. 最低客户端版本保证 V2 后停止 legacy projection，但旧行继续读到保留期结束。
8. migration 从当前 schema 开始可重复执行两次，结果必须一致。

## 十六、测试与验收

### 16.1 Rust 与采集器

- success、failure、cancel、deny、authorization error、abort 各有且只有一个 run 终态。
- run ID 在授权前产生；event sequence 严格递增。
- SafeFlash 覆盖设备检测、fastbootd、getvar、分区探测、每个 flash、skip、slot、wipe 与 reboot。
- 成功和非零退出都保留命令、输出、路径、serial、URL、分区与验证结果。
- 首个失败后不生成虚假后续命令；计划剩余步骤标记 `SKIPPED`。
- 1 MiB 以上输出按 UTF-8 边界分块并可逐字重组；缺块令 trace 不完整。

### 16.2 凭据防线

- 把密码、Bearer、Cookie、Set-Cookie、敏感 URL query、CLI secret flag、PEM/OpenSSH 私钥注入每个字段和输出流。
- 扫描 spool、HTTP body、D1、Worker logs 和管理员响应，凭据哨兵值必须为零命中。
- 非凭据 URL、路径、serial、argv 和工具上下文仍完整可见。

### 16.3 Reporter 与 API

- 重启重放未确认记录。
- 任意 chunk 失败不丢当前项或未尝试尾部。
- shutdown 真实 flush。
- 只有 ack 项被删除。
- 同用户重试幂等，跨用户 ID 冲突为 `409`。
- source IP 只来自 `CF-Connecting-IP`。
- 预期 event/chunk 未齐时拒绝把 run 标为 complete。

### 16.4 管理员 UI

- UTF-8 完整文档、零 console error、零乱码。
- 恰好六个菜单；点击、键盘、刷新、深链接与前进/后退保持唯一当前页。
- 五级下钻双向工作，URL、标题、面包屑、上下文和焦点同步。
- 所有字段来自后端，不在前端生成成功证据。
- loading、empty、partial、stale、unauthorized、error、retry 状态齐全。
- 危险 mutation 防重复提交，成功后刷新权威状态，错误保留上下文和重试入口。
- 320/360/768/1024/宽屏无 body 级溢出；代码区独立滚动。
- WCAG AA、44px 目标、键盘路径、对话框焦点和 reduced motion 自动检查通过。

### 16.5 端到端

- 模拟一次成功 VIVO 线刷：管理员看到每个成功分区、完整命令、stdout/stderr、退出码与后置验证。
- 模拟一次失败 VIVO 线刷：管理员看到具体失败分区、原因、命令上下文、停止边界和 skipped 后续步骤。
- 断网并重启客户端：恢复后上传相同 run，D1 无重复且 trace 完整。
- V1 与 V2 客户端并存：后台能区分并正确降级。

## 十七、实施顺序

正式实施拆成两个顺序依赖、可独立验收的 implementation plan：

- **Plan A：结构化追踪 V2**——完成 Rust 采集、凭据防线、spool、上传 API、D1、管理员查询 API 和 V1 降级；以 API/契约测试与端到端假运行作为交付物。
- **Plan B：管理员 Ops Console**——在 Plan A 的查询契约稳定后完全重写管理员前端；以六菜单、五级真实日志、危险操作、无障碍与响应式验收作为交付物。

Plan B 不得以 mock 数据替代尚未完成的 Plan A，也不得与 Plan A 并行猜测最终字段。

1. 新增 V2 domain 类型、凭据防线与序列化测试。
2. 新增进程执行器 trace decorator，并接入 OperationCoordinator 和 SafeFlash typed metadata。
3. 新增磁盘 spool、逐项 ack、重试、启动重放和 shutdown flush。
4. 新增 D1 migration 与 `/api/usage/traces/v2`，完成严格校验和幂等事务。
5. 新增管理员 V2 查询、输出分页、聚合与 admin audit。
6. 完全重写 `cloudflare/web/src/admin`，先完成 shell/路由/状态组件，再完成六页面。
7. 接入五级操作审计，删除所有 mock 数据和前端合成证据。
8. 增加 V1/V2 双读与旧记录降级。
9. 运行 Rust、Worker、前端、迁移、可访问性、窄屏和端到端验证。
10. 生产部署顺序：schema/API → 客户端 V2 → 管理后台 V2；任何一步失败均可回退到旧摘要视图。

## 十八、视觉原型的定位

`.superpowers/brainstorm/.../admin-ops-console-interactive.html` 是视觉与信息架构原型，只用于确认布局和交互层级。它不是生产文件，不是数据源，也不能作为测试通过的依据。正式实现必须使用真实 API、完整 UTF-8 文档、模块化文件、严格转义和权威服务端状态。
