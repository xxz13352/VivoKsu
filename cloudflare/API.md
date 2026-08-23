# api.nwflash.cc.cd —— API 文档

`api.nwflash.cc.cd` 是 Nwflash 的 ROM OTA 链接代理服务(Cloudflare Worker `nwflash-rom`)。它唯一持有 VOTA API Token,接收客户端的 **PD + 版本号**,转发到 VOTA 取回 OTA 下载链接。

- **Base URL**: `https://api.nwflash.cc.cd`
- **上游**: `https://api.otau.cc.cd`(VOTA,不对外暴露,不改动)
- **协议**: HTTPS + JSON(Cloudflare 边缘 TLS 1.3)
- **CORS**: 已允许跨域(`Access-Control-Allow-Origin: *`)
- **鉴权**: 可选 `Authorization: Bearer <API token>`(token 由后台「用户管理」生成;不带则记为匿名)
- **版本门禁**: 所有请求带 `X-Nwflash-Version` 头;版本低于后台「版本号控制」的最低版本 → **426 强制更新**(见 [版本门禁](#版本门禁强制更新))
- **日志**: 每次查询记入 D1(按用户),可在 `web.nwflash.cc.cd` 查看
- **在线会话**: 登录后客户端每 5s 心跳(`POST /api/heartbeat`)保持在线、接收强制下线;`GET /api/online` 查在线用户(显示名/时长)。管理端「在线状态」可强制下线。心跳数据存 D1 `online_sessions`,会话超过 120s 未心跳即视为离线(Worker Cron 兜底清理)
- **操作门禁**: 客户端每个用户操作运行前询问 `POST /api/operation/authorize`(默认放行;封禁/停用拒绝);执行后批量上传 `POST /api/usage/logs` 使用日志(按操作分类存储)
- **网络完整性**: 登录/活动心跳返回 Ed25519 签名短期租约;`GET /api/security/pins` 返回签名双 pin 清单;`POST /api/integrity/report` 接收严格脱敏、限流、幂等的最小事件

## 端点

### `GET /health`

健康检查。

**响应 200**
```json
{ "status": "ok", "source": "VotaApiRomSource" }
```

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `status` | string | 固定 `ok` |
| `source` | string | 数据源类型(当前恒为 `VotaApiRomSource`,即真实 VOTA 代理) |

---

### `GET /api/security/pins`

公共签名 pin 清单,仅授权主机 `api.nwflash.cc.cd`。响应含当前叶证书 SPKI pin 与 Google Trust Services WE1 中间证书备用 pin:

```json
{
  "pinset_payload": "<unpadded-base64url-json>",
  "pinset_signature": "<unpadded-base64url-ed25519-signature>"
}
```

`pinset_payload` 解码后固定包含:

```json
{
  "version": 1,
  "host": "api.nwflash.cc.cd",
  "not_before": 1787444740,
  "expires_at": 1788049600,
  "primary_pin": "kavrs5Bk3Tjn+0G+uPjWGBqJsXzW5kHFNPzgxuvrcKY=",
  "backup_pin": "kIdp6NNEd8wsugYyyIYFsi1ylMCED3hZbSR8ZFsa/A4="
}
```

`not_before` 为签发时刻前 60 秒,`expires_at` 为签发时刻后 7 天。签名输入是原始 `pinset_payload` ASCII。客户端必须验签并严格检查 host、version、有效期和两个 pin;服务端缺少/无法导入签名 secret 时返回 `503`。

---

### `POST /api/integrity/report`

接收退出前的最小完整性事件。可匿名调用;携带有效 bearer token 时绑定 `api_user_id` 并标记 trusted。携带了 Authorization 但 token 无效时返回 `401`;匿名事件始终是 untrusted telemetry,不能直接触发封号。

请求体最大 **4096 bytes**。Worker 先检查 `Content-Length`,再以流方式读取并在超限时立即取消,只有完整请求保持在上限内才执行 JSON 解析。

```json
{
  "event_id": "event-550e8400-e29b-41d4-a716-446655440000",
  "phase": "startup",
  "reason": "image_crc_invalid",
  "client_version": "1.4.0",
  "build_id": "build-2026-08-23",
  "occurred_at": 1787444800
}
```

只允许以上六个字段,缺字段或任何额外字段均返回 `400`。特别禁止 token、password、path、URL、serial 和 raw output。

- `phase` 闭集:`startup`、`login`、`session_restore`、`heartbeat`、`operation_admission`、`pin_validation`。
- `reason` 闭集:`image_crc_invalid`、`lease_signature_invalid`、`lease_binding_invalid`、`lease_expired`、`sequence_rollback`、`pin_mismatch`、`debugger_detected`、`virtual_machine_detected`、`authenticode_invalid`、`release_manifest_invalid`。
- `event_id`、`build_id` 只允许 URL-safe 标识字符;时间为正整数 epoch 秒。

服务端用单个 D1 transactional batch 完成临时 owner claim、条件配额递增、accepted event 写入和 owner-scoped claim 删除。claim 插入同时要求 `integrity_events` 尚无该 event ID;batch 最后一条语句按 event ID + 随机 claim token 删除临时行,因此提交后 `integrity_event_claims` 必须为空,over-quota 唯一 ID 也不会形成持久存储。并发重复请求只有在 durable `integrity_events` 行已存在时返回 `200 { "ok": true, "duplicate": true }`;over-quota 请求均返回 `429` 且不写 event。`integrity_rate_limits` 每个 IP/window 只有一行和一个 `last_event_id` 有界标记,同一 over-quota event 的并发竞争只计费一次,不同唯一 ID 不会扩展行数。若配额语句报错,整个 batch(含临时 claim/marker 更新)回滚,竞争请求可重新认领,不会观察到 provisional success。每个 IP 的 SHA-256 base64url 摘要按 60 秒窗口最多接受 20 个 event;首次 accepted 返回 `202`;D1 不保存原始 IP。请求体超限返回 `413`。

---

### `GET /api/app/version?current=<客户端版本>`

Nwflash **版本策略查询**(免登录,桌面端启动强制更新拦截用)。返回后台「版本号控制」的生效策略(启用的版本中最高者)。

**参数**

| 参数 | 必填 | 说明 | 示例 |
| --- | --- | --- | --- |
| `current` | 否 | 客户端当前版本号;缺省按 `0.0.0` 处理 | `1.0.0` |

**成功响应 200**
```json
{
  "latest": "1.2.0",
  "min": "1.0.0",
  "download_url": "https://example.com/Nwflash-1.2.0.zip",
  "update_required": true,
  "force_update": true
}
```

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `latest` | string \| null | 当前最新启用版本;后台未登记版本时为 `null` |
| `min` | string \| null | 允许的最低版本,低于此 `force_update = true` |
| `download_url` | string \| null | 更新下载链接(后台可配,可能为空) |
| `update_required` | bool | `current < latest`(有可更新版本) |
| `force_update` | bool | `current < min`(**必须更新,客户端应拦截**) |

---

### 版本门禁(强制更新)

**所有请求**(`/api/login`、`/api/me`、`/api/rom`)都必须携带客户端版本号,服务端每次校验:

| 头 | 必填 | 说明 |
| --- | --- | --- |
| `X-Nwflash-Version` | ✅ | 桌面端当前版本号(如 `1.0.0`)。低于后台「版本号控制」最低版本 → **426** |

**426 响应**(`code: UPDATE_REQUIRED`)
```json
{
  "error": "请更新 Nwflash 到最新版本后继续使用。",
  "code": "UPDATE_REQUIRED",
  "latest": "1.2.0",
  "min": "1.0.0",
  "download_url": "https://example.com/Nwflash-1.2.0.zip"
}
```

客户端收到 426 应弹强制更新窗(展示 `latest` / `min` / `download_url`),**无跳过路径**。

---

### `POST /api/login`

桌面端登录(商业工具门禁)。保留原账号/密码校验与版本门禁;成功后同时返回 API token 和 120 秒 Ed25519 签名租约。生产签名私钥只从 Worker secret `SESSION_SIGNING_PRIVATE_KEY_PKCS8` 导入;缺失或无效时返回 `503`,不回退到无签名响应。

**请求体**
```json
{
  "username": "demo",
  "password": "DemoPass123",
  "client_version": "1.4.0",
  "build_id": "build-2026-08-23",
  "process_nonce": "nonce-abc",
  "session_id": "session-abc"
}
```

**成功 200**
```json
{
  "ok": true,
  "token": "<64位hex>",
  "username": "demo",
  "name": "演示用户",
  "lease_payload": "<unpadded-base64url-json>",
  "lease_signature": "<unpadded-base64url-ed25519-signature>"
}
```

`lease_payload` 解码后的 UTF-8 JSON 字段固定为 snake_case:`version = 1`、`kind = "login"`、`username`、`token_sha256`、`client_version`、`build_id`、`process_nonce`、`session_id`、`sequence = 1`、`issued_at`、`expires_at`。`token_sha256` 是 bearer token 原始 UTF-8 字节 SHA-256 的无填充 base64url。签名输入是响应中**原始、未补 `=` 的 `lease_payload` ASCII 字节**,客户端必须先验签再解码 JSON。

服务端在请求字段、账号、密码、封禁和停用检查通过后先生成签名候选,再用 D1 `session_leases` 原子认领精确的 session ID、用户、client version、build ID、process nonce 和 `sequence = 1`;只有认领成功才返回 token/租约。session ID 已存在返回 `409`,不会返回另一个 token 或租约。缺失/畸形签名 key 返回 `503` 且不创建会话状态;签名服务故障期间既有 `400/401` 登录失败仍保持原语义。

**失败**:`401` —— `用户名或密码错误` / `账号已被封禁,请联系管理员。` / `账号已被停用。`;`400` 缺少或非法字段;`503` 签名服务不可用。

---

### `GET /api/me`

校验本地 token(记住登录)。带 `Authorization: Bearer <token>`。

**200**
```json
{ "loggedIn": true, "name": "演示用户" }
```
或 `{ "loggedIn": false }`(token 无效)。

---

### `POST /api/heartbeat`

**在线会话心跳**(登录后客户端每 5s 一次):保持「在线」并可接收服务端指令(强制下线 / 封禁 / 强制更新)。必须带 `Authorization: Bearer <token>`;也走版本门禁(低于最低版本 → 426,客户端弹更新窗)。

**活动心跳请求体**
```json
{
  "sessionId": "<客户端启动时生成的 GUID>",
  "clientVersion": "1.4.0",
  "active": true,
  "build_id": "build-2026-08-23",
  "process_nonce": "nonce-abc",
  "sequence": 41
}
```

| 字段 | 必填 | 说明 |
| --- | --- | --- |
| `sessionId` | ✅ | 客户端每次启动生成,标识本次会话(在线列表/踢人/时长以此为单位) |
| `clientVersion` | ✅ | 客户端版本号;也接受 snake_case `client_version` |
| `active` | 否 | `false` = goodbye:服务端删除该会话行(正常退出/强制退出前发送) |
| `build_id` | 活动时 ✅ | 当前受保护构建 ID |
| `process_nonce` | 活动时 ✅ | 本进程随机 nonce |
| `sequence` | 活动时 ✅ | 当前租约正整数序号;必须小于 JS 安全整数上限 |

**活动成功 200**
```json
{
  "ok": true,
  "force_exit": false,
  "lease_payload": "<unpadded-base64url-json>",
  "lease_signature": "<unpadded-base64url-ed25519-signature>"
}
```
```json
{ "ok": true, "force_exit": true, "reason": "违规下线" }
```

活动心跳必须与 D1 中的用户、session ID、client version、build ID、process nonce 和当前 sequence **全部精确匹配**。服务端先签名 `sequence + 1` 候选,再用完整绑定元组和旧 sequence 执行原子 compare-and-swap;CAS 同时要求 `online_sessions.force_exit_at` 仍为空,所以在读取后、CAS 前发生的强制退出也会阻止租约返回和 sequence 推进。只有 CAS 获胜才返回候选。并发同序号最多一个成功;重放、回退、跳号、绑定变更、未知 session 或跨用户 ownership 返回 `409` 且不返回签名字段。签名失败发生在 CAS 之前,因此不会推进服务端 sequence。per-token 3 秒限速返回 `429`,同样不签发、不推进。强制退出响应不创建新能力。

goodbye 只需 `sessionId` 和 `active = false`;它删除当前用户的 `session_leases` 与 `online_sessions` 行并返回 `{ "ok": true, "force_exit": false }`,不要求签名 secret,也不创建新租约。

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `force_exit` | bool | `true` = **服务端要求本进程立即退出**。客户端应停止心跳、弹窗提示 reason 后退出进程 |
| `reason` | string \| null | 强制下线原因(管理端填写,≤200 字符) |

**强制退出触发点**:管理端「在线状态」强制下线(下一个心跳 ≤5s 收到);账号被封禁;token 被停用/轮换(心跳返回 401/403)。kick 是**瞬态**(仅当前会话),持续封禁靠 `banned` 在登录与业务层阻断。

**服务端节流/配额防护**:每个有效活动心跳必须原子写入安全 sequence;在线展示投影的 `last_seen_at` 仍至少隔 60s 更新一次,且 `connected_at` 永不被更新。per-token 最小心跳间隔 3s;被限速请求返回 429,不会收到签名租约。stale `session_leases` 随在线会话清理窗口回收。

---

### `GET /api/online`

**在线用户列表(客户端视角)**:在线总数 + 各会话的显示名 / 版本 / 上线时间 / 时长。必须带 `Authorization: Bearer <token>`;**不返回登录 username / IP / user_id**(最小暴露:在线时段不给任意持 token 用户)。

**成功 200**
```json
{
  "count": 2,
  "sessions": [
    { "name": "演示用户", "client_version": "1.0.0", "connected_at": 1786700000, "last_seen_at": 1786703600, "duration_seconds": 3600, "is_self": true },
    { "name": "另一用户", "client_version": "1.0.0", "connected_at": 1786701000, "last_seen_at": 1786701000, "duration_seconds": 2600, "is_self": false }
  ]
}
```

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `count` | number | 在线会话数(last_seen 在 120s 窗口内) |
| `name` | string | 显示名(非登录账号) |
| `client_version` | string | 客户端版本 |
| `connected_at` / `last_seen_at` | number | **epoch 秒** |
| `duration_seconds` | number | 已在线时长(秒,自 `connected_at` 起算) |
| `is_self` | bool | 该会话是否为当前 token 的会话 |

完整列表(含 username/IP/session_id + 强制下线操作)在 `web.nwflash.cc.cd` 后台「在线状态」。在线判定窗口与 stale 清理见 `ONLINE_TIMEOUT_MS`(默认 120s,由 Worker Cron `*/3 * * * *` 兜底清理)。

---

### `POST /api/operation/authorize`

**操作许可门禁**:客户端**每个用户操作运行前**询问一次。服务端默认放行;账号被封禁/停用时拒绝。必须带 `Authorization: Bearer <token>`。

**请求体**
```json
{ "operation": "Flashing", "title": "正在刷写 boot" }
```

**成功 200**(默认放行)
```json
{ "allowed": true }
```

**拒绝 200**(封禁/停用)
```json
{ "allowed": false, "reason": "账号已被封禁,请联系管理员。" }
```

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `allowed` | bool | `false` = 拒绝该操作,客户端应阻止开始并提示 reason |
| `reason` | string \| null | 拒绝原因 |

> 客户端策略:服务端明确拒绝(封禁/停用)或 token 无效(401)→ 阻止操作;网络/服务端临时错误 → **默认放行**(服务端默认许可,不可达不应阻塞刷写;账号封禁由心跳 5s 内强制退出兜底)。

---

### `POST /api/usage/logs`

**使用日志批量上传**:客户端每次用户操作(刷写/重启/传输/ROOT…)执行完成后,把记录(操作分类/标题/结果/耗时)批量上传。服务端按 `operation_kind` 分类存储,归属用户由 token 解析。必须带 `Authorization: Bearer <token>`。

**请求体**
```json
{ "logs": [ { "operation": "Flashing", "title": "正在刷写 boot", "status": "success", "started_at": 1786700000, "ended_at": 1786700060, "duration_ms": 60000 } ] }
```

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `operation` | string | 操作分类(Flashing / Rebooting / Transferring / Installing…) |
| `title` | string | 操作标题 |
| `status` | string | `success` / `failed` / `canceled` |
| `started_at` / `ended_at` | number | epoch 秒 |
| `duration_ms` | number \| null | 耗时 |

**成功 200** `{ "ok": true, "received": 1 }`。单批最多 200 条。后台「使用日志」可查看/筛选。

---

### `GET /api/rom?pd=<PD>&version=<版本>`

按 **PD 码 + 版本号** 解析 OTA 下载链接。**必须携带登录 token**。所有请求也须带 `X-Nwflash-Version`(见 [版本门禁](#版本门禁强制更新))。

**请求头**

| 头 | 说明 |
| --- | --- |
| `Authorization: Bearer <token>` | **必填**。API 用户 token(登录或后台「用户管理」获取)。无 / 无效 → 401;封禁 → 403 |
| `X-Nwflash-Version` | **必填**。客户端版本号,低于后台最低版本 → 426 |

**参数**

| 参数 | 必填 | 说明 | 示例 |
| --- | --- | --- | --- |
| `pd` | ✅ | 设备 PD 码(`ro.product.device`) | `PD2417` |
| `version` | ✅ | 固件版本号 | `16.2.12.0.W10.V000L1` |

**成功响应 200**
```json
{
  "pd": "PD2417",
  "version": "16.2.12.0.W10.V000L1",
  "url": "https://sysuptxdl.vivo.com.cn/upgrade/oem/files/20260723141715...zip?sign=...&t=...",
  "name": null,
  "sizeBytes": null,
  "sha256": null
}
```

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `pd` | string | 回显请求的 PD |
| `version` | string | 回显请求的版本号 |
| `url` | string | OTA 包下载直链(带 `sign`/`t` 签名参数,**有时效性,拿到后尽快下载**) |
| `name` | string \| null | 包名(上游常为空) |
| `sizeBytes` | long \| null | 包大小字节(上游常为空) |
| `sha256` | string \| null | 包 SHA-256(上游常为空) |

> `url` 的 `sign`/`t` 参数由 VOTA 生成,可能有时效;一次查询拿到的链接应尽快使用。

**错误响应**(`Content-Type: application/json`)

| HTTP | `error` 示例 | 含义 |
| --- | --- | --- |
| `400` | `缺少 pd 或 version 查询参数。` | 缺参数 |
| `401` | `请先登录。` | 未携带 `Authorization: Bearer` token |
| `401` | `API token 无效或已停用。` | token 无效 / 账号被停用 |
| `403` | `账号已被封禁。` | 账号被封禁(后台操作) |
| `426` | `请更新 Nwflash 到最新版本后继续使用。` | **客户端版本低于后台最低版本,强制更新**(详见 [版本门禁](#版本门禁强制更新)) |
| `401` | `AUTH_FAIL` 文本 | VOTA 认证失败(worker 的 token 无效 / 被吊销) |
| `402` | `INSUFFICIENT_CREDITS` 文本 | 运营方(VOTA)账户信用点不足 —— 仅影响该版本解析,非用户计费 |
| `403` | `FORBIDDEN` 文本 | VOTA 拒绝(VOTA_VER 不在白名单等) |
| `404` | `record not found` | VOTA 平台无此 PD+版本记录 |
| `429` | `RATE_LIMITED` 文本 | 请求过于频繁 |
| `500` | `服务端未配置 VOTA 凭据。` / `内部错误。` | worker 缺 token / 未捕获异常 |
| `502` | `无法连接上游 ROM API。` / `上游返回异常。` | 连不上 VOTA 或上游响应异常 |

**示例**

```bash
curl "https://api.nwflash.cc.cd/api/rom?pd=PD2417&version=16.2.12.0.W10.V000L1"
# → 200,带真实 OTA 链接

curl "https://api.nwflash.cc.cd/api/rom?pd=PD2417&version=99.99"
# → 404 {"error":"record not found"}
```

## 上游计费 / 信用点(运营方成本,不向用户收费)

每次成功调用 `resolve_url` 扣 **1 信用点**;`resolve_flash_url`(线刷包)扣 **3 信用点**。信用点归属 **worker 所持 token 的账户(运营方)** —— 这是 Nwflash 运营方在上游 VOTA 的成本,由开发者承担,**不对 Nwflash 用户做任何扣点 / 按次计费**。用户只要登录即可查询,不限制次数。`record not found` / 参数错误不扣点。当前余额可在 VOTA 平台查看。

## 配置

非机密项在 `cloudflare/wrangler.toml` 的 `[vars]`:

| 变量 | 默认值 | 说明 |
| --- | --- | --- |
| `VOTA_BASE_URL` | `https://api.otau.cc.cd` | 上游地址 |
| `VOTA_ACTION` | `resolve_url` | `resolve_url`(OTA,-1)/ `resolve_flash_url`(线刷,-3) |
| `VOTA_VER` | `0.1.0` | 平台客户端版本白名单 |
| `HEARTBEAT_WRITE_INTERVAL_MS` | `60000` | 同一会话 `last_seen_at` 至少隔这么久写一次 D1(写节流,配额防护) |
| `ONLINE_TIMEOUT_MS` | `120000` | 在线判定窗口;超过未心跳的会话视为离线并被清理(API 与 web 后台一致) |
| `ONLINE_SESSION_CAP` | `3` | 每用户同时在线会话数上限(超出删最旧) |

**Cron**:`wrangler.toml [triggers]` 每 3 分钟跑一次 `scheduled()`,兜底清理 stale 会话行(客户端全崩溃时也可靠过期)。

机密(不进代码,`wrangler secret put` 设置):

| 机密 | 说明 |
| --- | --- |
| `VOTA_API_TOKEN` | VOTA 的 API Token(Authorization: Bearer)。**只在 worker 上,绝不下发客户端** |
| `SESSION_SIGNING_PRIVATE_KEY_PKCS8` | Ed25519 私钥 PKCS#8 DER 的**无填充 base64url**。只用于签名租约/pin 清单,不得记录、返回或提交仓库 |

## 部署

```bash
cd cloudflare
npm install
npx wrangler login
npx wrangler secret put VOTA_API_TOKEN    # 粘贴 token
npx wrangler secret put SESSION_SIGNING_PRIVATE_KEY_PKCS8
npm test                                  # 先跑 Worker 安全测试
npm run typecheck                         # strict tsc + Wrangler dry-run,不部署
npm run deploy                            # 先检查远端签名 secret,再部署
```

`npm run deploy` 的 `predeploy` 会读取远端 secret 清单,缺少 `SESSION_SIGNING_PRIVATE_KEY_PKCS8` 时失败。不要用直接 `wrangler deploy` 绕过预检。仓库不包含生产/测试 Ed25519 私钥;测试在运行时生成临时 WebCrypto 密钥材料,生产仅从 Env secret 导入为 non-extractable `CryptoKey`。

## 功能记录(Changelog)

| 日期 | 变更 |
| --- | --- |
| 2026-08-13 | 初始部署:worker `nwflash-rom`,自定义域 `api.nwflash.cc.cd`;`/health` + `/api/rom` 代理 VOTA `resolve_url`;token 存 worker 机密;错误映射 400/401/402/403/404/429/500/502 |
| 2026-08-13 | **接入后台系统**:共用 D1(`nwflash-db`);`/api/rom` 增加 版本号控制(未启用版本→404)、API 用户 token 认证(可选,无效→401)、按用户记访问日志。后台管理见 `web.nwflash.cc.cd`(登录 / 版本 / 用户 / 日志) |
| 2026-08-13 | **桌面端登录(商业工具)**:api_users 加 username/password(PBKDF2);新增 `POST /api/login`(账号密码→token)与 `GET /api/me`(校验 token);Nwflash 桌面端启动强制登录 |
| 2026-08-13 | **强制登录 + 封禁**:`/api/rom` 必须携带 token(无→401 请先登录);api_users 加 `banned`,封禁用户禁止登录与查询(登录 401 / 查询 403);后台支持封禁/解封 |
| 2026-08-13 | 明确商业模型:账号授权制 —— 用户登录即可查询、不按次计费;上游 VOTA 信用点由运营方承担 |
| 2026-08-14 | **Nwflash 版本门禁(强制更新)**:新增 `GET /api/app/version`(免登录策略查询);所有请求带 `X-Nwflash-Version` 头,低于后台最低版本 → **426 UPDATE_REQUIRED**;**移除 ROM 白名单** —— `/api/rom` 不再做 PD+版本门禁,登录即可解析任意版本 |
| 2026-08-14 | **在线会话心跳 + 强制下线**:D1 新增 `online_sessions` / `admin_audit_log`;`POST /api/heartbeat`(每 5s,检测强制下线/封禁/426)、`GET /api/online`(客户端视角在线列表,仅显示名/版本/时长,不含 username/IP);管理端「在线状态」可强制下线。服务端:per-token 心跳限速 + 每用户会话数上限 + 60s 写节流 + epoch 秒时间戳 + `last_seen` 索引 + Cron 兜底清理 |
| 2026-08-14 | **操作许可门禁 + 使用日志**:`POST /api/operation/authorize`(客户端每个用户操作运行前询问,默认放行、封禁/停用拒绝);`POST /api/usage/logs`(使用日志批量上传,按 `operation_kind` 分类存储);D1 新增 `usage_logs` 表;管理端「使用日志」查看/筛选 |
| 2026-08-23 | **签名租约 + pin 清单 + 完整性遥测**:登录/活动心跳签发绑定 token 摘要、build/process/session/sequence 的 Ed25519 租约;新增签名双 pin `/api/security/pins`;新增 4 KiB 严格遥测 `/api/integrity/report`,D1 event ID 幂等与 hash-IP 60s/20 次限流 |

## 管理后台

- 地址:`https://web.nwflash.cc.cd`(详见 `web/README.md`)。
- 功能:管理员登录、**Nwflash 版本控制(强制更新)**、API 用户管理(token 生成/轮换/停用)、访问日志查看、**在线状态(实时会话 + 强制下线)**、**使用日志(客户端操作分类)**。

## 代码结构

```
cloudflare/
├─ src/index.ts        # Worker 入口:路由 + resolveRom + 心跳/在线 + 完整性遥测
├─ src/security.ts     # Ed25519 签名、pin 清单、严格限长遥测解析
├─ test/security.test.ts # 实际安全 helper + Worker/D1 边界测试
├─ test/security.workerd.test.ts # 实际 Workerd Worker route + D1 并发集成测试
├─ vitest.workerd.config.ts # @cloudflare/vitest-plugin 独立配置 + 运行时临时密钥/迁移
├─ wrangler.toml       # 变量与自定义域路由 + Cron 触发器
├─ README.md           # 部署/使用说明
└─ API.md              # 本文档(接口契约)
```
