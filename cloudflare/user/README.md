# user.nwflash.cc.cd —— Nwflash 用户自助门户

面向 **Nwflash 授权客户**的个人账户后台(Cloudflare Worker `nwflash-user`,高级白 + 毛玻璃设计),与 `api.nwflash.cc.cd` / `web.nwflash.cc.cd` 共用 D1 数据库 `nwflash-db`。

## 功能

| 功能 | 说明 |
| --- | --- |
| **登录** | 账号 + 密码(自含 PBKDF2,与桌面端同账号);返回该用户的 API token,后续请求带 `Authorization: Bearer` |
| **我的查询日志** | 只看**自己**的 `access_logs`(PD / 版本 / 状态 / URL),分页 + PD 过滤;✓ OKAY / ✕ FAILED 双墨 |
| **在线会话** | 自己账户的桌面端在线会话(版本 / IP / 上线 / 时长),实时「当前在线」身份片,**⟠ 强制下线**仅限本人会话 |
| **修改密码** | 校验当前密码后更新(≥6 位,新旧不可相同) |

## 界面「高级白 + 毛玻璃」

客户面向表面 = **冷白画布 `#F5F7F9` + 磨砂玻璃卡片**(与暗色管理台形成明暗双面):

- **磨砂玻璃**(Double-Bezel):外层衬底壳 + 内层 `linear-gradient(150deg, rgba(255,255,255,.78), rgba(255,255,255,.5))` + `backdrop-filter: blur(18px) saturate(160%)` + 内顶高光。
- **单一深青强调 `#0E7A6F`**(在线/激活/OKAY/主操作)+ rose `#C23E38`(危险/强制下线);零 em-dash;系统字体栈(Segoe UI Variable + Cascadia Mono)。
- **签名**:顶栏实时「当前在线 · N 会话」身份片(青点呼吸,reduced-motion 静止)+ ⟠ 玫瑰急停。
- WCAG AA(ink 14.6 / faint 4.6 / teal 4.9 / rose 4.8)、焦点环、`aria-live`、移动端折叠。

## 目录

```
user/
├─ wrangler.toml       # D1 绑定 + 自定义域 user.nwflash.cc.cd
├─ src/index.ts        # Worker:登录 + 自助 API + 鉴权 + 安全头 + SPA 托管
└─ src/user.html       # 用户门户单页(登录 / 我的日志 / 在线会话 / 修改密码)
```

## API(user.nwflash.cc.cd)

| 端点 | 方法 | 鉴权 | 说明 |
| --- | --- | --- | --- |
| `/api/login` | POST | 免 | `{username,password}` → `{ok, token, username, name}` |
| `/api/me` | GET | Bearer | 用户信息 + `online`(活跃会话数) |
| `/api/me/logs?limit&offset&pd` | GET | Bearer | 本人 `access_logs`,返回 `{logs, total}` |
| `/api/me/password` | POST | Bearer | `{current, newPassword}` → 校验当前密码后更新 |
| `/api/me/sessions` | GET | Bearer | 本人在线会话(含 `force_exit`) |
| `/api/me/sessions/kick` | POST | Bearer | `{sessionId}` → 仅限本人会话设 `force_exit` |

> 鉴权 = 与桌面端同源的 API token(`api_users.token`);写操作额外校验 `X-Requested-With: XMLHttpRequest`(CSRF 兜底)。

## 安全(与 api/web 同规格)

- 全站 HTTPS(Cloudflare 边缘 TLS 1.3)、HSTS、CSP(`script-src 'self' 'unsafe-inline'`)、`X-Frame-Options: DENY`、no-store。
- 密码 PBKDF2-SHA256(100k 迭代)+ 随机盐;token 仅存 `api_users`,门户只在本浏览器会话持有(可「记住登录」存 localStorage)。
- **越权隔离**:所有 `/api/me/*` 查询与写操作按 `api_user_id` 过滤,用户只能看/改自己账户的数据。

## 部署

```bash
cd cloudflare/user
npx wrangler deploy          # 绑定自定义域 user.nwflash.cc.cd
```

> D1(`nwflash-db`)已由 api / web 建好,用户门户只读/写同一库,无需额外建表。

## 功能记录

| 日期 | 变更 |
| --- | --- |
| 2026-08-14 | 初始上线:Worker `nwflash-user` + 自定义域 `user.nwflash.cc.cd`;高级白 + 毛玻璃门户;登录 / 我的日志 / 在线会话(⟠ 强制下线)/ 修改密码;PBKDF2 + Bearer token + CSRF |
