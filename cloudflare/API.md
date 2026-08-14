# api.nwflash.cc.cd —— API 文档

`api.nwflash.cc.cd` 是 VivoKsu 的 ROM OTA 链接代理服务(Cloudflare Worker `nwflash-rom`)。它唯一持有 VOTA API Token,接收客户端的 **PD + 版本号**,转发到 VOTA 取回 OTA 下载链接。

- **Base URL**: `https://api.nwflash.cc.cd`
- **上游**: `https://api.otau.cc.cd`(VOTA,不对外暴露,不改动)
- **协议**: HTTPS + JSON(Cloudflare 边缘 TLS 1.3)
- **CORS**: 已允许跨域(`Access-Control-Allow-Origin: *`)
- **鉴权**: 可选 `Authorization: Bearer <API token>`(token 由后台「用户管理」生成;不带则记为匿名)
- **版本门禁**: 所有请求带 `X-VivoKsu-Version` 头;版本低于后台「版本号控制」的最低版本 → **426 强制更新**(见 [版本门禁](#版本门禁强制更新))
- **日志**: 每次查询记入 D1(按用户),可在 `web.nwflash.cc.cd` 查看

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

### `GET /api/app/version?current=<客户端版本>`

VivoKsu **版本策略查询**(免登录,桌面端启动强制更新拦截用)。返回后台「版本号控制」的生效策略(启用的版本中最高者)。

**参数**

| 参数 | 必填 | 说明 | 示例 |
| --- | --- | --- | --- |
| `current` | 否 | 客户端当前版本号;缺省按 `0.0.0` 处理 | `1.0.0` |

**成功响应 200**
```json
{
  "latest": "1.2.0",
  "min": "1.0.0",
  "download_url": "https://example.com/VivoKsu-1.2.0.zip",
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
| `X-VivoKsu-Version` | ✅ | 桌面端当前版本号(如 `1.0.0`)。低于后台「版本号控制」最低版本 → **426** |

**426 响应**(`code: UPDATE_REQUIRED`)
```json
{
  "error": "请更新 VivoKsu 到最新版本后继续使用。",
  "code": "UPDATE_REQUIRED",
  "latest": "1.2.0",
  "min": "1.0.0",
  "download_url": "https://example.com/VivoKsu-1.2.0.zip"
}
```

客户端收到 426 应弹强制更新窗(展示 `latest` / `min` / `download_url`),**无跳过路径**。

---

### `POST /api/login`

桌面端登录(商业工具门禁)。提交账号密码,成功返回该用户的 **API token**(供 `/api/rom` 用)。

**请求体**
```json
{ "username": "demo", "password": "DemoPass123" }
```

**成功 200**
```json
{ "ok": true, "token": "<64位hex>", "username": "demo", "name": "演示用户" }
```

**失败**:`401` —— `用户名或密码错误` / `账号已被封禁,请联系管理员。` / `账号已被停用。`;`400` 缺参数。

---

### `GET /api/me`

校验本地 token(记住登录)。带 `Authorization: Bearer <token>`。

**200**
```json
{ "loggedIn": true, "name": "演示用户" }
```
或 `{ "loggedIn": false }`(token 无效)。

---

### `GET /api/rom?pd=<PD>&version=<版本>`

按 **PD 码 + 版本号** 解析 OTA 下载链接。**必须携带登录 token**。所有请求也须带 `X-VivoKsu-Version`(见 [版本门禁](#版本门禁强制更新))。

**请求头**

| 头 | 说明 |
| --- | --- |
| `Authorization: Bearer <token>` | **必填**。API 用户 token(登录或后台「用户管理」获取)。无 / 无效 → 401;封禁 → 403 |
| `X-VivoKsu-Version` | **必填**。客户端版本号,低于后台最低版本 → 426 |

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
| `426` | `请更新 VivoKsu 到最新版本后继续使用。` | **客户端版本低于后台最低版本,强制更新**(详见 [版本门禁](#版本门禁强制更新)) |
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

每次成功调用 `resolve_url` 扣 **1 信用点**;`resolve_flash_url`(线刷包)扣 **3 信用点**。信用点归属 **worker 所持 token 的账户(运营方)** —— 这是 VivoKsu 运营方在上游 VOTA 的成本,由开发者承担,**不对 VivoKsu 用户做任何扣点 / 按次计费**。用户只要登录即可查询,不限制次数。`record not found` / 参数错误不扣点。当前余额可在 VOTA 平台查看。

## 配置

非机密项在 `cloudflare/wrangler.toml` 的 `[vars]`:

| 变量 | 默认值 | 说明 |
| --- | --- | --- |
| `VOTA_BASE_URL` | `https://api.otau.cc.cd` | 上游地址 |
| `VOTA_ACTION` | `resolve_url` | `resolve_url`(OTA,-1)/ `resolve_flash_url`(线刷,-3) |
| `VOTA_VER` | `0.1.0` | 平台客户端版本白名单 |

机密(不进代码,`wrangler secret put` 设置):

| 机密 | 说明 |
| --- | --- |
| `VOTA_API_TOKEN` | VOTA 的 API Token(Authorization: Bearer)。**只在 worker 上,绝不下发客户端** |

## 部署

```bash
cd cloudflare
npm install
npx wrangler login
npx wrangler secret put VOTA_API_TOKEN    # 粘贴 token
npx wrangler deploy                       # 绑定 api.nwflash.cc.cd
```

## 功能记录(Changelog)

| 日期 | 变更 |
| --- | --- |
| 2026-08-13 | 初始部署:worker `nwflash-rom`,自定义域 `api.nwflash.cc.cd`;`/health` + `/api/rom` 代理 VOTA `resolve_url`;token 存 worker 机密;错误映射 400/401/402/403/404/429/500/502 |
| 2026-08-13 | **接入后台系统**:共用 D1(`nwflash-db`);`/api/rom` 增加 版本号控制(未启用版本→404)、API 用户 token 认证(可选,无效→401)、按用户记访问日志。后台管理见 `web.nwflash.cc.cd`(登录 / 版本 / 用户 / 日志) |
| 2026-08-13 | **桌面端登录(商业工具)**:api_users 加 username/password(PBKDF2);新增 `POST /api/login`(账号密码→token)与 `GET /api/me`(校验 token);VivoKsu 桌面端启动强制登录 |
| 2026-08-13 | **强制登录 + 封禁**:`/api/rom` 必须携带 token(无→401 请先登录);api_users 加 `banned`,封禁用户禁止登录与查询(登录 401 / 查询 403);后台支持封禁/解封 |
| 2026-08-13 | 明确商业模型:账号授权制 —— 用户登录即可查询、不按次计费;上游 VOTA 信用点由运营方承担 |
| 2026-08-14 | **VivoKsu 版本门禁(强制更新)**:新增 `GET /api/app/version`(免登录策略查询);所有请求带 `X-VivoKsu-Version` 头,低于后台最低版本 → **426 UPDATE_REQUIRED**;**移除 ROM 白名单** —— `/api/rom` 不再做 PD+版本门禁,登录即可解析任意版本 |

## 管理后台

- 地址:`https://web.nwflash.cc.cd`(详见 `web/README.md`)。
- 功能:管理员登录、**VivoKsu 版本控制(强制更新)**、API 用户管理(token 生成/轮换/停用)、访问日志查看。

## 代码结构

```
cloudflare/
├─ src/index.ts        # Worker 入口:路由 + resolveRom + 错误映射
├─ wrangler.toml       # 变量与自定义域路由
├─ README.md           # 部署/使用说明
└─ API.md              # 本文档(接口契约)
```
