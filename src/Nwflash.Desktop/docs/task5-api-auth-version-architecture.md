# Task5 API/会话/版本契约迁移架构说明（nwflash-infrastructure + nwflash-tauri）

本任务只处理 Task 2 对应的会话门禁与版本链路部分：`Cloudflare API` 契约消费、`/api/login`、`/api/me`、`/api/heartbeat`、`/api/online`、`/api/operation/authorize`、`/api/usage/logs`、`/api/rom`、`/api/app/version`。

目标约束来自以下基线：
- `archive/csharp/docs/migration-baselines/2026-08-16-wpf-behavior-baseline.md`
- `archive/csharp/docs/migration-baselines/api-contract-cases.md`
- `src/Nwflash.Desktop/docs/task4-domain-model-architecture.md`

不改 `cloudflare/**`，所有行为以现有契约为真模型，优先保证“行为等价 + 可测试”。

---

## 一、职责边界（按 crate 划分）

### 1) `nwflash-infrastructure`

- 文件：
  - `src/api_client.rs`
  - `src/auth.rs`
  - `src/version_client.rs`
  - `src/api_model.rs`
  - `tests/api_contract.rs`
  - `tests/auth_contract.rs`
  - `tests/version_contract.rs`
- 职责：
  - 负责所有 `api.nwflash.cc.cd` 的 HTTP 合约调用。
  - 建模请求/响应结构体：登录、会话、在线状态、版本检查、ROM、心跳、操作授权、使用日志上传。
  - 实现统一错误模型与状态码映射。
- 禁止：
  - 不持久化 token、不管理进程状态、不直接触发 UI 退出。

### 2) `nwflash-tauri`

- 文件：
  - `src/commands/auth.rs`
  - `src/commands/version.rs`
  - `src/commands/mod.rs`
  - `src/lib.rs`
  - `src/main.rs`
- 职责：
  - 将 `nwflash-infrastructure` 的合同方法封装为可被前端调用的 Tauri 命令。
  - 维护会话内存态（`session_token: RwLock<Option<String>>`）。
  - 暴露 API：`auth_login`、`auth_logout`、`auth_validate_token`、`version_check`。
- 禁止：
  - 不在命令层实现刷写/设备流程，不做业务编排。

---

## 二、关键数据流

### 1) 启动版本检查（`version_check`)

1. 前端启动时调用 `version_check`。
2. `nwflash-tauri::version_check` 调用 `VersionClient::check`。
3. `VersionClient` 通过 `CloudflareClient` 请求 `GET /api/app/version?current=<version>`。
4. 返回 `VersionCheckResponse`：
   - `latest/min_version/download_url/update_required/force_update`。
5. 非 200 或网络失败按 WPF baseline 语义降级，允许启动继续（`force_update` 由接口决定）。

### 2) 登录与会话注入（`auth_login` + state）

1. 前端调用 `auth_login(username, password)`。
2. `AuthService::login` 调用 `POST /api/login`，校验非 200 即失败（含 400/401/426 等）。
3. 成功时返回 `{ token, username, name }`。
4. `auth_login` 将 `token` 写入 `AppState.session_token`，仅保留进程内存态。

### 3) token 校验（`auth_validate_token`）

1. 前端调用 `auth_validate_token`。
2. `session_token` 为空则返回 `None`。
3. 有 token 时调用 `AuthService::validate_token`（实为 `CloudflareClient::validate_token`）。
4. 返回 `Option<String>`：
   - `Some(name)`：token 有效。
   - `None`：token 无效或服务端拒绝（401/403 等在基础层降级为 `None`）。
5. `UpdateRequired` 不吞，按错误上抛，交给调用侧做更新流程。

### 4) 版本/会话/操作相关错误映射

- 426：统一映射为更新要求（`UpdateRequired`）。
- 401/403：在 `validate_token` 和 `api_client` 流程中视为会话失效路径，交给上层（会话层）处理强制退出。
- 400/402/404/429/5xx：转为 `CloudflareError` 带中文提示信息，保持可重试语义。
- `/api/online` 的 `sessions` 非数组或字段缺失时，返回空列表并不抛错。
- `pd/version` 查询使用 URL 编码，避免特殊字符注入/截断。

---

## 三、`api_client.rs` 关键契约点（Task5 实现摘要）

- `CloudflareClient::login`：返回 `AuthSession`（`token/username/name`），保留原始错误到 `CloudflareError`。
- `CloudflareClient::check_update`：支持字段缺省回退，并对空体返回可空字段的结果。
- `CloudflareClient::validate_token`：401/403 不抛业务异常，返回 `Ok(None)`；仅 426 透传更新。
- `CloudflareClient::resolve_rom`：统一对 `pd`、`version` 做 URL 编码。
- `CloudflareClient::post_heartbeat`：
  - `goodbye` 与常规心跳共用超时参数；
  - 失败时可返回 `force_exit`/`reason`。
- `CloudflareClient::get_online`：`sessions` 非数组安全回退空列表。
- `CloudflareClient::authorize_operation` 与 `upload_usage_logs`：按基线语义透传允许/拒绝与最小影响上报路径。

---

## 四、Tauri 命令层状态机

- `AppState`：
  - `client`、`auth_service`、`version_client` 为共享客户端句柄。
  - `session_token` 为进程内状态，`RwLock` 保护。
- `run_app(context)`：
  - 注入 `APP_LABEL` 和窗口标题；
  - 仅注册鉴权/版本命令入口。
- `logout`：
  - 直接清理 `session_token`，命令返回 `LogoutResult { ok: true }`。

---

## 五、测试与验证

本任务的验收测试点全部放在 infra crate，当前记录如下：

- `cargo test -p nwflash-infrastructure --test api_contract`（12 条）
  - 覆盖：版本检查、登录、在线会话、心跳、ROM 查询、操作授权、日志上报、字符编码、错误映射。
- `cargo test -p nwflash-infrastructure --test auth_contract`（4 条）
  - 覆盖：登录成功/失败、token 有效返回、token 失效返回 `None`。
- `cargo test -p nwflash-infrastructure --test version_contract`（2 条）
  - 覆盖：版本查询与网络失败降级。
- `cargo test -p nwflash-tauri --lib --no-run`
  - 确认 tauri 入口库与命令签名编译通过。

---

## 六、当前状态与下一步（Task6 前提）

- Task5 的 API 契约层已对齐为“可执行迁移基线”：
  - 合约结构完整；
  - 错误与版本更新分支可用；
  - 命令桥可供前端会话层与版本流程调用。
- 下一步需在 `nwflash-tauri` 的会话/生命周期/任务协调层继续接入：
  - `OperationCoordinator`、`Heartbeat`、`SessionLifecycle`（Task3）；
  - 与 `auth_validate_token`/`heartbeat` 的 426/401/403/force_exit 事件路由。
