# VivoKsu 项目索引

> **导航地图**:从这一页定位所有文档、代码、服务与数据。
> 快速上手看 [README](../README.md);深入理解看 [架构文档](architecture.md)。

## 一、这是什么

**VivoKsu** —— Vivo 手机刷机 / Root 商业付费工具。Windows WPF 桌面应用(.NET 8)+ **100% Cloudflare 托管后端**(Workers + D1,零自有服务器)。

- 桌面端启动**强制登录**(账号由后台创建),登录后即可使用,不对用户扣点 / 限次。
- 拿 ROM 链路:`api.nwflash.cc.cd`(Worker)持 VOTA 凭据 → 版本授权校验 → 返回 OTA 直链 → 桌面端下载解包刷写。
- 运营闭环:账号授权 + 版本开关 + 封禁 / 停用 + 按用户审计,全部在后台「固件登记簿」完成。

## 二、文档地图

| 文档 | 看什么 | 入口 |
| --- | --- | --- |
| **项目总览** | 功能页面、技术栈、构建 / 发布 / 部署、商业模式 | [../README.md](../README.md) |
| **架构文档** | 系统总览、桌面端模块职责、Worker / D1、数据流、设计决策与踩坑 | [architecture.md](architecture.md) |
| **API 契约** | `api.nwflash.cc.cd` 端点、参数、错误码、计费、功能记录 | [../cloudflare/API.md](../cloudflare/API.md) |
| **后台文档** | `web.nwflash.cc.cd`「固件登记簿」界面、安全、部署、功能记录 | [../cloudflare/web/README.md](../cloudflare/web/README.md) |
| **线刷文档** | VIVO 安全刷写流程、三种 OTA 格式、下载 / 解包 / 刷写细节 | [safeflash-ota.md](safeflash-ota.md) |
| **Worker 部署** | `cloudflare/` 部署、机密、变量 | [../cloudflare/README.md](../cloudflare/README.md) |
| 设计与计划 | 早期 UI 设计 / 计划草稿(历史) | [superpowers/](superpowers/) |

## 三、代码地图

```
VivoKsu 工具/
├─ src/VivoKsu.App/          # 桌面应用(net8.0-windows · WPF)
│  ├─ App.xaml(.cs)          # 启动门禁 / 崩溃日志 / 退出清理
│  ├─ MainWindow.xaml        # 单窗口多页面导航 + 全部 XAML
│  ├─ LoginWindow.xaml(.cs)  # 登录窗口(taste 风格)
│  ├─ Models/                # 领域模型(AppPage / 分区 / payload / 快照 / 日志)
│  ├─ ViewModels/            # 各页面 MVVM 视图模型
│  ├─ Services/              # 组合根 AppComposition + 业务服务 / 基础设施
│  ├─ apk/ platform-tools/ payload-tools/ root-tools/ scrcpy/   # 内置组件
├─ cloudflare/               # 后端(TypeScript Worker + D1)
│  ├─ src/index.ts           # api.nwflash.cc.cd · Worker nwflash-rom
│  ├─ web/src/index.ts       # web.nwflash.cc.cd · Worker nwflash-web(API + 托管 SPA)
│  ├─ web/src/admin.html     # 「固件登记簿」后台单页(三菜单)
│  └─ wrangler.toml          # D1 绑定 + 自定义域 + vars
├─ tests/VivoKsu.App.Tests/  # 桌面应用单元测试(267 用例)
├─ scripts/                  # Publish-Release.ps1 / Ensure-Scrcpy.ps1 / verify-*.ps1
└─ docs/                     # 本文档 + architecture.md + safeflash-ota.md
```

## 四、服务与数据

| 域 | Worker | 角色 |
| --- | --- | --- |
| `api.nwflash.cc.cd` | `nwflash-rom` | 桌面登录(`/api/login`)、ROM 查询(`/api/rom`,强制 token + 版本控制 + 记日志) |
| `web.nwflash.cc.cd` | `nwflash-web` | 管理控制台:管理员登录 / 版本号控制 / 用户管理 / 访问日志 |

**D1 `nwflash-db`** 由两个 Worker 共用:

| 表 | 用途 |
| --- | --- |
| `admins` / `admin_sessions` | 后台管理员 + 会话(PBKDF2 + HttpOnly Cookie) |
| `api_users` | 客户端账号 = 桌面登录账号(username + password + token + enabled + banned) |
| `versions` | 版本号控制(pd + version + enabled) |
| `access_logs` | 每次 `/api/rom` 查询的审计(用户 / PD / 版本 / URL / 状态) |

## 五、常见任务 → 去哪

| 想做什么 | 打开 |
| --- | --- |
| 改后台界面 / 三菜单 | `cloudflare/web/src/admin.html`(单文件 SPA,内联 CSS/JS) |
| 加 / 改 API 端点 | `cloudflare/src/index.ts` + 同步 [API.md](../cloudflare/API.md) |
| 改后台 API / 安全头 | `cloudflare/web/src/index.ts` |
| 建表 / 迁移 D1 | `cloudflare/web/schema.sql` + `npx wrangler d1 execute` |
| 改桌面某页面 | `src/VivoKsu.App/ViewModels/` + `MainWindow.xaml` |
| 改登录门禁 / token | `App.xaml.cs` · `LoginService.cs` · `OtaApiClient.cs` · `ToolPathPreferences.cs` |
| 改刷写 / 设备能力 | `src/VivoKsu.App/Services/`(`FastbootCliRunner` 唯一 fastboot 后端) |
| 跑桌面测试 | `dotnet test tests/VivoKsu.App.Tests/…csproj` |
| 发布桌面端 | `scripts/Publish-Release.ps1` → `artifacts/release/` |
| 部署 Worker | `cd cloudflare && npx wrangler deploy`(web 子目录同理) |

## 六、当前状态(2026-08-14)

- **后端**:全部 Cloudflare;旧自建 .NET 服务端已删除;api / web 均已部署。
- **后台**:「固件登记簿」控制台(三菜单 + 服务健康带 + № 登记册 + 撕口 token 凭证 + OKAY/FAIL 协议回显)已上线。
- **桌面端**:264 测试全绿;登录门禁 + 强制登录 + 封禁 / 版本控制接线完整。
