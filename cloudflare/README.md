# Nwflash Cloudflare Worker(ROM OTA 代理 + 商业门禁)

> **接口契约见 [API.md](API.md)** —— 端点、参数、响应、错误码、计费、功能记录。
> **后台管理见 [web/README.md](web/README.md)** —— `web.nwflash.cc.cd`。
> **用户门户见 [user/README.md](user/README.md)** —— `user.nwflash.cc.cd`。
> **官网见 [website/README.md](website/README.md)** —— `nwflash.cc.cd`。

Nwflash 的**整个服务端都托管在 Cloudflare,零自有服务器**:

- **官网**:Worker `nwflash-site`,部署在 **`nwflash.cc.cd`** —— 对外营销落地页(高级白 + 液态玻璃,产品介绍 / 功能 / 更新日志)。
- **API**:Worker `nwflash-rom`,部署在 **`api.nwflash.cc.cd`** —— 登录、版本控制、ROM 解析、访问日志、**在线会话(心跳 / 强制下线 / 在线列表)**。
- **后台**:Worker `nwflash-web`,部署在 **`web.nwflash.cc.cd`** —— 管理员 / 版本 / 用户 / 日志 / **在线状态(强制下线)** 管理。
- **用户门户**:Worker `nwflash-user`,部署在 **`user.nwflash.cc.cd`** —— 授权客户自助后台(我的日志 / 在线会话 / 修改密码)。
- **数据库**:D1 `nwflash-db`,API / 后台 / 用户门户三个 Worker 共用(官网不连 D1)。

桌面应用 `OtaApiClient.DefaultBaseUrl` 默认就是 `https://api.nwflash.cc.cd`,无需配置、无需本地起 .NET 服务端。

上游仍是 **VOTA `https://api.otau.cc.cd`**(不改动),Worker 只做代理,凭据(VOTA_API_TOKEN)留在服务端。**上游信用点由运营方承担,不对用户扣点计费**。

## 端点(`api.nwflash.cc.cd`)

| 端点 | 说明 |
| --- | --- |
| `GET /health` | 健康检查 |
| `POST /api/login` | 账号密码 → API token + Ed25519 签名登录租约;`revoked:*` marker 先 CAS 为唯一的新 32-byte hex token |
| `GET /api/me` | 校验 token 有效性;已替换/撤销/停用 token 返回 401(桌面端每次强制登录,不再用于免登录) |
| `POST /api/heartbeat` | D1 完整绑定 + sequence/最小间隔/force-exit 单点 CAS;仅 CAS 获胜返回严格递增签名租约 |
| `GET /api/security/pins` | `api.nwflash.cc.cd` 的签名叶证书 + WE1 备用 SPKI pin 清单 |
| `POST /api/integrity/report` | 匿名/鉴权最小完整性事件(4 KiB 上限、闭集字段、IP 限流、event ID 幂等) |
| `GET /api/online` | 在线用户列表(鉴权;显示名/版本/时长,不含 username/IP) |
| `POST /api/operation/authorize` | 操作许可门禁(每个用户操作运行前询问;默认放行、封禁/停用拒绝) |
| `POST /api/usage/logs` | 使用日志批量上传(按操作分类存储) |
| `GET /api/rom?pd=X&version=Y` | 解析 OTA 直链(**强制登录** + 版本控制 + 记日志) |

错误映射:NOT_FOUND/`not found`→404, AUTH_FAIL→401, INSUFFICIENT_CREDITS→402, FORBIDDEN→403, RATE_LIMITED→429, 其它→502。

## 首次部署

```bash
cd cloudflare
npm install
npx wrangler login            # 浏览器登录 Cloudflare 账户(域名 nwflash.cc.cd 需已在账户内)
npx wrangler secret put VOTA_API_TOKEN    # 粘贴 VOTA 的 API Token
npx wrangler secret put SESSION_SIGNING_PRIVATE_KEY_PKCS8  # 无填充 base64url PKCS#8 Ed25519 DER
npm test
npm run typecheck             # strict tsc + Wrangler dry-run,不会部署
npm run test:workerd          # 实际 Workerd Worker route + 隔离 D1 集成套件
npm run deploy                # 预检远端签名 secret 后部署
```

> D1(`nwflash-db`)建库 / 建表见 [web/README.md](web/README.md);`/api/rom` 依赖 D1 做版本控制与访问日志。
> 必须先应用 `web/schema.sql` 中的 `session_leases`、`integrity_event_claims`、`integrity_events` 与 `integrity_rate_limits` 表。`npm run deploy` 缺少 `SESSION_SIGNING_PRIVATE_KEY_PKCS8` 时失败;不要直接调用 `wrangler deploy` 绕过预检。
> `npm test` 保留 Node + controlled D1 fake 的确定性边界测试;`npm run test:workerd` 使用 `@cloudflare/vitest-plugin` 在实际 Workerd runtime 中运行 API Worker route、带 HTML Text module rule 的生产后台 Worker 模块和共享隔离 D1。集成配置每次从 `web/schema.sql` 生成临时迁移并在运行时生成临时 Ed25519 key,不读取或写入生产 secret/remote D1。

部署后验证:

```bash
curl "https://api.nwflash.cc.cd/health"
curl "https://api.nwflash.cc.cd/api/rom?pd=PD2417&version=16.2.12.0.W10.V000L1"
# 不带 token → 401 请先登录
```

## 商业模型

账号授权制:**用户登录即可查询,不对用户扣点 / 限制次数**。账号由 `web.nwflash.cc.cd` 后台创建(用户名 + 密码 + token);`/api/rom` 强制登录,封禁用户 `403`;每次查询按用户记日志。上游 VOTA 信用点扣的是 **Worker 所持 token 账户(运营方)**,是运营成本。

## 机密

- `VOTA_API_TOKEN` 用 secret 存,不进代码。
- `SESSION_SIGNING_PRIVATE_KEY_PKCS8` 是 Ed25519 PKCS#8 DER 的无填充 base64url,只从 Worker Env 导入为 non-extractable signing key;缺失/无效时登录、活动心跳和 pin 清单返回 `503`,不回退到无签名能力。仓库不保存固定测试 seed、PKCS#8 fixture 或任何私钥。
- 非机密项在 `wrangler.toml [vars]`:
  `VOTA_BASE_URL`(默认 https://api.otau.cc.cd)、`VOTA_ACTION`(resolve_url / resolve_flash_url)、`VOTA_VER`(0.1.0)。
