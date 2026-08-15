# Nwflash 项目索引

> **导航地图**:从这一页定位所有文档、代码、服务与数据。
> 快速上手看 [README](../README.md);深入理解看 [架构文档](architecture.md)。

## 一、这是什么

**奶蛙Flash(代码名 NWflash)** —— Vivo 手机刷机 / Root 商业付费工具。Windows WPF 桌面应用(.NET 8)+ **100% Cloudflare 托管后端**(Workers + D1,零自有服务器)。

> **命名约定**:客户端 UI 统一显示 **「奶蛙Flash」**;代码 / 工程 / 服务名用 **NWflash**,缩写 **NWF**,域名 `nwflash.cc.cd`,API 版本头 `X-Nwflash-Version`。详见 [架构文档 §命名约定](architecture.md#命名约定2026-08-14定稿)。

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
| **用户门户文档** | `user.nwflash.cc.cd`「我的账户」界面、API、安全、部署 | [../cloudflare/user/README.md](../cloudflare/user/README.md) |
| **线刷文档** | VIVO 安全刷写流程、三种 OTA 格式、下载 / 解包 / 刷写细节 | [safeflash-ota.md](safeflash-ota.md) |
| **Worker 部署** | `cloudflare/` 部署、机密、变量 | [../cloudflare/README.md](../cloudflare/README.md) |

> 界面整合设计过程文档(2026-08-15):[设计规格](superpowers/specs/2026-08-15-ui-consolidation-design.md) · [实施计划](superpowers/plans/2026-08-15-ui-consolidation.md)。

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
│  ├─ apk/ platform-tools/ payload-tools/ root-tools/ scrcpy/   # 内置组件(apk/payload/scrcpy 发布外置)
├─ src/VivoKsu.Bootstrapper/ # 原生 AOT 引导器(.NET 首启检测/静默装)入口
├─ cloudflare/               # 后端(TypeScript Worker + D1)
│  ├─ src/index.ts           # api.nwflash.cc.cd · Worker nwflash-rom(登录/版本/ROM/心跳/在线)
│  ├─ web/src/index.ts       # web.nwflash.cc.cd · Worker nwflash-web(API + 托管 SPA)
│  ├─ web/src/admin.html     # 「固件登记簿」后台单页(版本/用户/日志/在线/使用日志五菜单)
│  ├─ user/src/index.ts      # user.nwflash.cc.cd · Worker nwflash-user(用户自助 API)
│  ├─ user/src/user.html     # 「我的账户」用户门户(我的日志/在线会话/修改密码)
│  ├─ website/src/index.html # nwflash.cc.cd 官网(高级白液态玻璃单页)
│  ├─ website/src/index.ts   # nwflash.cc.cd · Worker nwflash-site(托管官网)
│  └─ wrangler.toml          # D1 绑定 + 自定义域 + vars + Cron
├─ tests/VivoKsu.App.Tests/  # 桌面应用单元测试(392 用例)
├─ scripts/                  # Publish-Release.ps1 / Upload-Resources.ps1 / verify-*.ps1
└─ docs/                     # 本文档 + architecture.md + safeflash-ota.md
```

## 四、服务与数据

| 域 | Worker | 角色 |
| --- | --- | --- |
| `api.nwflash.cc.cd` | `nwflash-rom` | 桌面登录(`/api/login`)、ROM 查询(`/api/rom`,强制 token + 版本门禁 + 记日志)、版本策略(`/api/app/version`) |
| `web.nwflash.cc.cd` | `nwflash-web` | 管理控制台:管理员登录 / Nwflash 版本控制(强制更新) / 用户管理 / 访问日志 |
| `user.nwflash.cc.cd` | `nwflash-user` | **用户自助门户**:我的查询日志 / 在线会话(可强制下线) / 修改密码 |
| `nwflash.cc.cd` | `nwflash-site` | **官网**:产品介绍 / 功能 / 更新日志(高级白液态玻璃落地页) |

**D1 `nwflash-db`** 由三个 Worker 共用:

| 表 | 用途 |
| --- | --- |
| `admins` / `admin_sessions` | 后台管理员 + 会话(PBKDF2 + HttpOnly Cookie) |
| `api_users` | 客户端账号 = 桌面登录账号(username + password + token + enabled + banned) |
| `app_versions` | Nwflash 版本控制(version + min_version + download_url + enabled) |
| `access_logs` | 每次 `/api/rom` 查询的审计(用户 / PD / 版本 / URL / 状态) |

## 五、常见任务 → 去哪

| 想做什么 | 打开 |
| --- | --- |
| 改后台界面 / 五菜单 | `cloudflare/web/src/admin.html`(单文件 SPA,内联 CSS/JS) |
| 改官网页面 | `cloudflare/website/src/index.html`(液态玻璃单页,内联 CSS/JS) |
| 改用户门户 / 自助界面 | `cloudflare/user/src/user.html` + `cloudflare/user/src/index.ts` |
| 加 / 改 API 端点 | `cloudflare/src/index.ts` + 同步 [API.md](../cloudflare/API.md) |
| 改后台 API / 安全头 | `cloudflare/web/src/index.ts` |
| 改心跳 / 在线 / 强制下线 / 操作门禁 | 服务端 `cloudflare/src/index.ts`(心跳/在线/授权/使用日志)+ `cloudflare/web/src/index.ts`(kick/使用日志);客户端 `HeartbeatService.cs` · `OnlineViewModel.cs` · `OperationCoordinator.cs` · `AppComposition.cs` |
| 建表 / 迁移 D1 | `cloudflare/web/schema.sql` + `npx wrangler d1 execute` |
| 改桌面某页面 | `src/VivoKsu.App/ViewModels/` + `MainWindow.xaml` |
| 改登录门禁 / token | `App.xaml.cs` · `LoginService.cs` · `OtaApiClient.cs` · `ToolPathPreferences.cs` |
| 改刷写 / 设备能力 | `src/VivoKsu.App/Services/`(`FastbootCliRunner` 唯一 fastboot 后端) |
| 跑桌面测试 | `dotnet test tests/VivoKsu.App.Tests/…csproj` |
| 发布桌面端 | `scripts/Publish-Release.ps1` → `artifacts/release/` |
| 部署 Worker | `cd cloudflare && npx wrangler deploy`(web / user 子目录同理) |

## 六、当前状态(2026-08-15)

- **后端**:全部 Cloudflare;旧自建 .NET 服务端已删除;api / web / user / website 均已部署。
- **后台**:「固件登记簿」控制台(五菜单 + 服务健康带 + № 登记册 + 撕口 token 凭证 + OKAY/FAIL 协议回显)已上线。
- **桌面端**:392 测试全绿;登录门禁 + 强制登录 + 封禁 / 版本控制接线完整。
- **发布瘦身(2026-08-15)**:framework-dependent + 原生 AOT 引导器(`VivoKsu.Launcher.exe`,.NET 缺失→微软直链静默装);scrcpy / ROOT 管理器 APK / payload_dumper 外置按需下载(GitHub Release + gh-proxy.com/ghfast.top/ghproxy.net 多镜像 failover + SHA-256 校验);驱动 / adb-fastboot / magiskboot 保留随包。解压 ~205MB → ~24MB,zip ~95MB → ~11MB。详情见 [architecture.md §9](architecture.md)。
- **界面整合(2026-08-15)**:左侧菜单按刷机链路分组;左下角账号/时间/登出栏(同进程登出回登录窗,刷写中登出禁用);全部主进度统一到右上角「操作进度」区;文件管理传出弹保存位置对话框。设计文档见 [设计规格](superpowers/specs/2026-08-15-ui-consolidation-design.md) / [实施计划](superpowers/plans/2026-08-15-ui-consolidation.md)。
- **在线会话**:客户端每 5s 心跳保持在线;后台「在线状态」实时查看会话(用户/版本/IP/时长)并**强制下线**(≤5s 内客户端退出);客户端「在线状态」页查看在线用户与时长;心跳 force_exit / 封禁 / 426 均走防变砖退出(刷写中先取消、等 Idle 再退)。
- **操作门禁 + 使用日志**:客户端每个用户操作运行前经服务端 `POST /api/operation/authorize` 许可(默认放行、封禁/停用拒绝);执行后批量上传使用日志,后台「使用日志」按操作分类查看。
- **软件菜单 + 驱动安装**:「软件」页展示 Nwflash 版本 / USB 驱动 / scrcpy / payload_dumper 就绪状态;启动检测到未装 vivo USB 驱动时弹窗提醒,一键以管理员权限静默安装(pnputil 通配符递归装 ADB / fastboot / 联发科驱动 + 写 adb_usb.ini)。
- **用户门户**:`user.nwflash.cc.cd` 客户自助后台(高级白 + 毛玻璃)——我的查询日志 / 在线会话(⟠ 强制下线)/ 修改密码,已上线。
- **官网**:`nwflash.cc.cd` 官网(高级白 + 液态玻璃 + 高斯模糊)单页已上线,含产品介绍 / 功能 / 更新日志。
