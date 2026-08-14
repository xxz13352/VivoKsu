# web.nwflash.cc.cd —— ROM 服务后台管理

VivoKsu ROM 服务的管理控制台(Cloudflare Worker `nwflash-web`),与 `api.nwflash.cc.cd` 共用 D1 数据库 `nwflash-db`。

## 功能

| 功能 | 说明 |
| --- | --- |
| **登入界面** | 管理员账号密码登录(会话 Cookie,HttpOnly + Secure);首次用种子密码建初始管理员后立即改密 |
| **VivoKsu 版本控制** | 登记 VivoKsu 客户端版本号(版本 / 最低版本 / 下载地址),启用的最高版本为当前策略;**客户端版本低于「最低版本」→ 服务端 426 强制更新拦截**(所有请求带 `X-VivoKsu-Version` 校验) |
| **用户管理** | 创建 API 用户 → 生成 API token(仅显示一次);支持换 token / 停用 / **封禁** / 删除。封禁后该用户无法登录与查询 |
| **用户日志记录** | 每次 `api.nwflash.cc.cd/api/rom` 查询都记入 D1(用户/PD/版本/状态/链接/时间),可按用户、PD 过滤,分页 |
| **在线状态** | 实时查看客户端在线会话(用户/版本/IP/上线/最后心跳/时长);**强制下线** = 给会话打 `force_exit`,客户端下一个心跳(≤5s)收到后退出进程;kick 动作写 `admin_audit_log` 审计 |

## 界面「固件登记簿」(2026-08 重写)

后台为最高规格 taste 重写的系统控制台(设计方向经 4 方向 + 3 对抗评审胜出):

- **恰好四个菜单** —— 版本号控制 / 用户管理 / 访问日志 / **在线状态(LIVE)**;**改密为头部维护按钮,不是第五菜单**。
- **视觉语言**:机加工纸面画布 + 发丝刻线 + 单一账簿蓝;数据一律等宽 mono;无阴影堆叠、无渐变。系统字体栈(Segoe UI Variable + Cascadia Mono)—— Google Fonts 在中国大陆不可达,且被 CSP 拦。
- **服务健康带**:VivoKsu 当前版本 / API 用户 / **在线人数** / 近 24h 查询 / 近 24h 失败(客户端 best-effort 统计,基于最近 500 条日志)。
- **VivoKsu 版本控制**:登记版本号(版本 / 最低版本 / 下载地址)→ № 页边码登记册 + 双墨状态(● 启用 / ○ 停用)+ 当前策略结算(客户端版本低于「最低版本」→ 强制更新)。
- **用户管理**:建号 → **撕口一次性 token 凭证**(可复制);重置密码 / 换 token / 封禁 / 停用 / 删除。
- **访问日志**:带列标尺的查询读出口,OKAY / FAIL 双墨。
- **在线状态**:实时会话登记册(显示名 + 登录账号 / 版本 / IP / 上线 / 最后心跳 / 在线时长),每 10s 刷新;**强制下线**按钮给会话打 force_exit(原因可选,≤200 字符,显示在客户端)。
- **操作反馈以 OKAY/FAIL/INFO 协议行回显** —— 每次操作都写成协议行,操作历史即审计轨迹。

## 安全(最高规格)

- 全站 HTTPS,Cloudflare 边缘 **TLS 1.3**(AES-256-GCM)。
- `HSTS: max-age=31536000; includeSubDomains`、CSP、`X-Frame-Options: DENY`、`X-Content-Type-Options: nosniff`、`Referrer-Policy`、`Permissions-Policy`。
- 密码 **PBKDF2-SHA256**(100k 迭代)+ 随机盐;会话 token 随机 64 hex,Cookie HttpOnly + Secure + SameSite=Lax。
- http → https 301 跳转。
- **CSRF 兜底**:所有状态变更 API(`/api/online/kick`、改密、版本、用户管理)校验 `X-Requested-With: XMLHttpRequest` 头(admin.html 的 fetch 一律携带),跨站表单无法伪造。

## 目录

```
web/
├─ wrangler.toml       # D1 绑定 + 自定义域 web.nwflash.cc.cd + vars
├─ schema.sql          # D1 表结构(admins / admin_sessions / api_users / app_versions / access_logs / online_sessions / admin_audit_log)
├─ src/index.ts        # Worker:admin API + 鉴权 + 安全头 + SPA 托管 + 在线/强制下线
└─ src/admin.html      # 后台单页(「固件登记簿」:登录 / 版本 / 用户 / 日志 / 在线状态)
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

- **桌面端登录**:VivoKsu **每次启动强制登录** → 用「登录账号 + 密码」调 `api.nwflash.cc.cd/api/login` → 拿到 API token 解锁应用(token 不持久化,仅本次会话有效)。启动时先查 `/api/app/version`,版本过低弹强制更新窗。
- **API 调用**:应用登录后,`/api/rom` 请求带 `Authorization: Bearer <token>` 与 `X-VivoKsu-Version`,后台日志按该用户记录。
- **版本门禁**:所有请求带客户端版本号;后台「VivoKsu 版本控制」设的最低版本高于客户端 → 服务端 426,客户端弹强制更新。

> **商业模型**:账号授权制 —— 用户登录即可查询,不对用户扣点 / 限制次数;上游 VOTA 信用点为运营方成本。

## D1 数据

| 表 | 用途 |
| --- | --- |
| `admins` | 管理员(PBKDF2 哈希) |
| `admin_sessions` | 后台登录会话 |
| `api_users` | API 用户与 token |
| `app_versions` | VivoKsu 版本控制(version + min_version + download_url + enabled) |
| `access_logs` | 访问日志 |

## 功能记录

| 日期 | 变更 |
| --- | --- |
| 2026-08-13 | 初始部署:worker `nwflash-web` + 自定义域 `web.nwflash.cc.cd`;管理员登录(会话 Cookie)/ 版本号控制 / 用户管理 / 访问日志;最高规格加密(HTTPS/HSTS/CSP/PBKDF2) |
| 2026-08-14 | **界面重写「固件登记簿」**:taste 最高规格(4 方向 + 3 对抗评审胜出)——三菜单 + 改密头部按钮、服务健康带、№ 登记册 + 活结算、撕口 token 凭证、OKAY/FAIL/INFO 协议回显;WCAG AA 对比度 + tabs ARIA + 移动端适配;系统字体栈(国内 Google Fonts 不可达) |
| 2026-08-14 | **「版本号控制」改为 VivoKsu 版本控制 + 强制更新**:`/api/app-versions` CRUD;版本登记册(版本/最低版本/下载地址)+ 当前策略结算;**移除 ROM 白名单**(`versions` 表删除,`/api/rom` 不再做 PD+版本门禁) |
| (规划) | 配额限制、订阅计费等 |
