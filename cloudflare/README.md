# VivoKsu Cloudflare Worker(ROM OTA 代理 + 商业门禁)

> **接口契约见 [API.md](API.md)** —— 端点、参数、响应、错误码、计费、功能记录。
> **后台管理见 [web/README.md](web/README.md)** —— `web.nwflash.cc.cd`。

VivoKsu 的**整个服务端都托管在 Cloudflare,零自有服务器**:

- **API**:Worker `nwflash-rom`,部署在 **`api.nwflash.cc.cd`** —— 登录、版本控制、ROM 解析、访问日志、**在线会话(心跳 / 强制下线 / 在线列表)**。
- **后台**:Worker `nwflash-web`,部署在 **`web.nwflash.cc.cd`** —— 管理员 / 版本 / 用户 / 日志 / **在线状态(强制下线)** 管理。
- **数据库**:D1 `nwflash-db`,两个 Worker 共用。

桌面应用 `OtaApiClient.DefaultBaseUrl` 默认就是 `https://api.nwflash.cc.cd`,无需配置、无需本地起 .NET 服务端。

上游仍是 **VOTA `https://api.otau.cc.cd`**(不改动),Worker 只做代理,凭据(VOTA_API_TOKEN)留在服务端。**上游信用点由运营方承担,不对用户扣点计费**。

## 端点(`api.nwflash.cc.cd`)

| 端点 | 说明 |
| --- | --- |
| `GET /health` | 健康检查 |
| `POST /api/login` | 账号密码 → API token(桌面端登录门禁) |
| `GET /api/me` | 校验 token 有效性(桌面端每次强制登录,不再用于免登录) |
| `POST /api/heartbeat` | 在线会话心跳(登录后每 5s;检测强制下线 / 封禁 / 426) |
| `GET /api/online` | 在线用户列表(鉴权;显示名/版本/时长,不含 username/IP) |
| `GET /api/rom?pd=X&version=Y` | 解析 OTA 直链(**强制登录** + 版本控制 + 记日志) |

错误映射:NOT_FOUND/`not found`→404, AUTH_FAIL→401, INSUFFICIENT_CREDITS→402, FORBIDDEN→403, RATE_LIMITED→429, 其它→502。

## 首次部署

```bash
cd cloudflare
npm install
npx wrangler login            # 浏览器登录 Cloudflare 账户(域名 nwflash.cc.cd 需已在账户内)
npx wrangler secret put VOTA_API_TOKEN    # 粘贴 VOTA 的 API Token
npx wrangler deploy           # 部署并绑定自定义域 api.nwflash.cc.cd
```

> D1(`nwflash-db`)建库 / 建表见 [web/README.md](web/README.md);`/api/rom` 依赖 D1 做版本控制与访问日志。

部署后验证:

```bash
curl "https://api.nwflash.cc.cd/health"
curl "https://api.nwflash.cc.cd/api/rom?pd=PD2417&version=16.2.12.0.W10.V000L1"
# 不带 token → 401 请先登录
```

## 商业模型

账号授权制:**用户登录即可查询,不对用户扣点 / 限制次数**。账号由 `web.nwflash.cc.cd` 后台创建(用户名 + 密码 + token);`/api/rom` 强制登录,封禁用户 `403`;每次查询按用户记日志。上游 VOTA 信用点扣的是 **Worker 所持 token 账户(运营方)**,是运营成本。

## 机密

- `VOTA_API_TOKEN` 用 secret 存,不进代码。非机密项在 `wrangler.toml [vars]`:
  `VOTA_BASE_URL`(默认 https://api.otau.cc.cd)、`VOTA_ACTION`(resolve_url / resolve_flash_url)、`VOTA_VER`(0.1.0)。
