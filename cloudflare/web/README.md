# web.nwflash.cc.cd —— ROM 服务后台管理

VivoKsu ROM 服务的管理控制台(Cloudflare Worker `nwflash-web`),与 `api.nwflash.cc.cd` 共用 D1 数据库 `nwflash-db`。

## 功能

| 功能 | 说明 |
| --- | --- |
| **登入界面** | 管理员账号密码登录(会话 Cookie,HttpOnly + Secure);首次用种子密码建初始管理员后立即改密 |
| **版本号控制** | 管理允许解析的 PD + 版本(启用/停用/删除);**只有启用的版本 api 才会返回 OTA 链接**,否则 404 |
| **用户管理** | 创建 API 用户 → 生成 API token(仅显示一次);支持换 token / 停用 / **封禁** / 删除。封禁后该用户无法登录与查询 |
| **用户日志记录** | 每次 `api.nwflash.cc.cd/api/rom` 查询都记入 D1(用户/PD/版本/状态/链接/时间),可按用户、PD 过滤,分页 |

## 界面「固件登记簿」(2026-08 重写)

后台为最高规格 taste 重写的系统控制台(设计方向经 4 方向 + 3 对抗评审胜出):

- **恰好三个菜单** —— 版本号控制 / 用户管理 / 访问日志;**改密为头部维护按钮,不是第四菜单**。
- **视觉语言**:机加工纸面画布 + 发丝刻线 + 单一账簿蓝;数据一律等宽 mono;无阴影堆叠、无渐变。系统字体栈(Segoe UI Variable + Cascadia Mono)—— Google Fonts 在中国大陆不可达,且被 CSP 拦。
- **服务健康带**:启用版本 / API 用户 / 近 24h 查询 / 近 24h 失败(客户端 best-effort 统计,基于最近 500 条日志)。
- **版本号控制**:№ 页边码登记册 + 双墨状态(● 启用 / ○ 停用)+ 活结算页脚。
- **用户管理**:建号 → **撕口一次性 token 凭证**(可复制);重置密码 / 换 token / 封禁 / 停用 / 删除。
- **访问日志**:带列标尺的查询读出口,OKAY / FAIL 双墨。
- **操作反馈以 OKAY/FAIL/INFO 协议行回显** —— 每次操作都写成协议行,操作历史即审计轨迹。

## 安全(最高规格)

- 全站 HTTPS,Cloudflare 边缘 **TLS 1.3**(AES-256-GCM)。
- `HSTS: max-age=31536000; includeSubDomains`、CSP、`X-Frame-Options: DENY`、`X-Content-Type-Options: nosniff`、`Referrer-Policy`、`Permissions-Policy`。
- 密码 **PBKDF2-SHA256**(100k 迭代)+ 随机盐;会话 token 随机 64 hex,Cookie HttpOnly + Secure + SameSite=Lax。
- http → https 301 跳转。

## 目录

```
web/
├─ wrangler.toml       # D1 绑定 + 自定义域 web.nwflash.cc.cd
├─ schema.sql          # D1 表结构(admins / admin_sessions / api_users / versions / access_logs)
├─ src/index.ts        # Worker:admin API + 鉴权 + 安全头 + SPA 托管
└─ src/admin.html      # 后台单页(「固件登记簿」:登录 / 版本 / 用户 / 日志)
```

## 部署

```bash
# 1. 建库(一次性)
npx wrangler d1 create nwflash-db
# 把返回的 database_id 填入 wrangler.toml

# 2. 建表(一次性)
npx wrangler d1 execute nwflash-db --remote --file=web/schema.sql

# 3. 部署 + 初始管理员
cd web
npx wrangler secret put ADMIN_SEED_PASSWORD    # 初始密码;首次请求会自动创建 admin 用户
npx wrangler deploy                            # 绑定 web.nwflash.cc.cd
```

登录 `https://web.nwflash.cc.cd`(账号 `admin`,初始密码),用头部「改密」按钮修改密码(非菜单项)。

> 种子只生效一次:库内已有管理员后 `ADMIN_SEED_PASSWORD` 不再创建。稳定后可移除该 secret。

## API 用户与桌面端登录

在「用户管理」创建用户时填 **登录账号 + 初始密码**(同时生成 API token,仅显示一次):

- **桌面端登录**:VivoKsu **每次启动强制登录** → 用「登录账号 + 密码」调 `api.nwflash.cc.cd/api/login` → 拿到 API token 解锁应用(token 不持久化,仅本次会话有效)。
- **API 调用**:应用登录后,`/api/rom` 请求带 `Authorization: Bearer <token>`,后台日志按该用户记录。
- 版本必须先在此后台启用,否则 `/api/rom` 返回 404。

> **商业模型**:账号授权制 —— 用户登录即可查询,不对用户扣点 / 限制次数;上游 VOTA 信用点为运营方成本。

## D1 数据

| 表 | 用途 |
| --- | --- |
| `admins` | 管理员(PBKDF2 哈希) |
| `admin_sessions` | 后台登录会话 |
| `api_users` | API 用户与 token |
| `versions` | 版本号控制(pd + version + enabled) |
| `access_logs` | 访问日志 |

## 功能记录

| 日期 | 变更 |
| --- | --- |
| 2026-08-13 | 初始部署:worker `nwflash-web` + 自定义域 `web.nwflash.cc.cd`;管理员登录(会话 Cookie)/ 版本号控制 / 用户管理 / 访问日志;最高规格加密(HTTPS/HSTS/CSP/PBKDF2) |
| 2026-08-14 | **界面重写「固件登记簿」**:taste 最高规格(4 方向 + 3 对抗评审胜出)——三菜单 + 改密头部按钮、服务健康带、№ 登记册 + 活结算、撕口 token 凭证、OKAY/FAIL/INFO 协议回显;WCAG AA 对比度 + tabs ARIA + 移动端适配;系统字体栈(国内 Google Fonts 不可达) |
| (规划) | 配额限制、订阅计费等 |
