# 2026-08-16 Cloudflare 契约测试清单（桌面端迁移）

> 来源：`cloudflare/API.md` + `src/VivoKsu.App.Services` + 单元测试。
> 目的：在 Rust 侧逐条覆盖 API 契约与状态机，不跨写 cloudflare 源码。

## 全局约束

- Base URL：`https://api.nwflash.cc.cd`
- 全局头：`X-Nwflash-Version`（每个请求都带，`AppInfo.Version`）。
- 可选鉴权：`Authorization: Bearer <token>`（按端点要求是否必填）。
- 版本过低统一返回 426 + `UPDATE_REQUIRED`，客户端必须更新并退出。
- `cloudflare/**` 不参与迁移；仅按现有契约消费。

## 用例总表（Endpoint -> 测试矩阵）

### 1) `GET /api/app/version?current=<客户端版本>`

- 目的：启动强制更新拦截，仅在 `force_update=true` 时阻断启动。
- 成功字段：`latest, min, download_url, update_required, force_update`
- 错误行为（WPF 现状）：网络异常时返回“放行”（AllowAll）继续启动。
- Rust 验收条件：
  - 能构建并返回结构化结果。
  - 解析字段缺失时安全回退为 `null/false`。
  - 失败路径返回空动作（不触发硬失败）。
- 关联实现点：`src/VivoKsu.App/Services/AppVersionService.cs`，`tests/VivoKsu.App.Tests/AppVersionControlTests.cs`

### 2) `POST /api/login`

- 请求：`{ "username": "<用户名>", "password": "<密码>" }`
- 成功：`200` + token/name/username
- 失败：`400`、`401`（含“用户名或密码错误 / 被禁用 / 被封禁”语义）
- 426：进入强制更新
- Rust 验收条件：
  - 只允许 `200` 作为成功；
  - 非 200 都抛可读错误，404/500 等都不走 JSON 解析崩溃；
  - 缺少 token 时返回明确错误。
- 关联：`src/VivoKsu.App/Services/LoginService.cs`，`tests/VivoKsu.App.Tests/Login` 相关场景（登录失败文案与异常类型）

### 3) `GET /api/me`

- 作用：本地 token 续期与账号显示名校验。
- 成功：`loggedIn=true` 返回 `name`；失败：`loggedIn=false` 或异常。
- Rust 验收条件：
  - token 无效/离线时返回 null，不阻塞“需要重登”流程。
  - 426 继续走更新窗。

### 4) `GET /api/online`

- 请求：必须带 `Authorization: Bearer <token>`。
- 成功：`{ count, sessions: [...] }`
- 字段重点：`name, client_version, connected_at, last_seen_at, duration_seconds, is_self`
- Rust 验收条件：
  - sessions 非数组时返回空列表（不抛）；
  - `count` 可不严格绑定返回列表长度；
  - 单条时间字段兼容 number/string。
- 关联：`src/VivoKsu.App/Services/OtaApiClient.cs#getOnline` + `tests/.../OnlineViewModelTests.cs`

### 5) `POST /api/heartbeat`

- 请求体：`{ sessionId, clientVersion, active }`，常态 `active=true`，退出使用 `active=false`（goodbye）。
- 成功：`{ ok, force_exit, reason }`
- 业务映射：
  - `force_exit=true`：触发强制下线流程；
  - `401/403`：token 失效/封禁，强制退出；
  - `426`：先退出并弹更新窗；
  - 网络抖动：静默恢复（不退出）。
- Rust 验收条件：
  - `heartbeat` 每 5s 一次；
  - `goodbye` 使用 3s timeout；
  - 426/401/403 后循环停止；
  - 非 JSON 响应在 401/403 下仍能按状态映射失败（避免 JSON 解析异常阻塞）。
- 关联：`src/VivoKsu.App/Services/HeartbeatService.cs` + `tests/VivoKsu.App.Tests/HeartbeatServiceTests.cs`

### 6) `POST /api/operation/authorize`

- 请求：`{ operation, title }`
- 成功：`200 { allowed: true }`
- 拒绝：`200 { allowed: false, reason }`
- Rust 验收条件：
  - 直接拒绝时不执行后续刷写；
  - 401/403 由 caller 按强制退出策略处理；
- 关联：`src/VivoKsu.App/Services/OtaApiClient.cs`、`src/VivoKsu.App/Services/OperationCoordinator.cs`

### 7) `POST /api/usage/logs`

- 请求：`{ logs: [{ operation, title, status, started_at, ended_at, duration_ms, event_id }] }`
- 用途：成功/失败/取消后的异步批量上报。
- Rust 验收条件：
  - 空列表短路不发；
  - 批量成功后清空缓存；
  - 上传失败为 best-effort（不要阻塞业务终止）。
- 关联：`src/VivoKsu.App/Services/UsageLogUploader.cs` + `OperationCoordinator` + `tests/VivoKsu.App.Tests/UsageLogUploaderTests.cs`

### 8) `GET /api/rom?pd=<PD>&version=<版本>`

- `Authorization` 必填；缺失/失效通常是 `401`，封禁为 `403`（VOTA 层含 `AUTH_FAIL` / `INSUFFICIENT_CREDITS` / `FORBIDDEN`）。
- 常见失败码映射：
  - `404` `record not found`
  - `400` 缺参数
  - `402` 信用点不足
  - `403` 封禁
  - `426` 更新
  - `429` 限流
- Rust 验收条件：
  - `pd/version` 做 URL Encode；
  - `url/pd/version` 反序列化完整；
  - 400/402/403/404/429 映射中文提示不抛空异常；
  - 426 映射 `UpdateRequiredException`。
- 关联：`src/VivoKsu.App/Services/OtaApiClient.cs#ResolveAsync` + `tests/VivoKsu.App.Tests/OtaApiClientTests.cs`

## WPF 到 Tauri 的错误映射（Rust 事件层）

| HTTP/异常 | 既定行为 |
|---|---|
| 426 | 触发更新窗口，退出；无跳过 |
| 401/403（登录/心跳/操作） | 登录失败或强制退出/注销 |
| 400/404/429/5xx | 显示业务中文错误后允许页面继续，可重试 |
| 网络异常（登录后某些接口） | 在会话校验场景可降级为“未登录”；在刷写场景不影响已决资源逻辑 |

## 需要对齐的现有常量（迁移时保持一致）

- Heartbeat 频率：`5s`
- 心跳请求超时：`10s`
- goodbye 超时：`3s`
- `AppDomain` crash log 文件：`%LOCALAPPDATA%\\VivoKsu\\crash.log`
- 统一进度优先级（见 `docs/migration-baselines/2026-08-16-wpf-behavior-baseline.md#4-统一进度与状态优先级关键对照`)
- 最大内存日志条数：`500`

## 产物输出约束（Rust 迁移时）

- 先做 API/模型层单元测试，确保上述每条有对应断言再进入业务页。
- 只有当:
  1. 登录流程、
  2. 心跳更新退出链路、
  3. 操作门禁与日志上报、
  4. 进度/退出按钮禁用态
  全部与 WPF 基线一致时，才推进到刷写/镜像等大页迁移。
