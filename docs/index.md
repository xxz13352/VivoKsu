# Nwflash 项目索引

> **导航地图**:从这一页定位所有文档、代码、服务与数据。
> 快速上手看 [README](../README.md);深入理解看 [当前项目架构](project-architecture.md)。

## 一、这是什么

**奶蛙Flash(代码名 NWflash)** —— Vivo 手机刷机 / Root 商业付费工具。当前桌面端是 **React + Tauri + Rust**，后端为 **100% Cloudflare 托管**(Workers + D1,零自有服务器)；WPF 目录仅保留迁移历史与视觉基线。

> **命名约定**:客户端 UI 统一显示 **「奶蛙Flash」**;代码 / 工程 / 服务名用 **NWflash**,缩写 **NWF**,域名 `nwflash.cc.cd`,API 版本头 `X-Nwflash-Version`。详见 [架构文档 §命名约定](architecture.md#命名约定2026-08-14定稿)。

- 桌面端启动**强制登录**(账号由后台创建),登录后即可使用,不对用户扣点 / 限次。
- **WPF 既有 ROM 链路**:`api.nwflash.cc.cd`(Worker)持 VOTA 凭据 → 版本授权校验 → 返回 OTA 直链 → WPF 桌面端下载解包刷写。当前 Tauri/Rust 的公开 IPC 边界见下文，不向 React 暴露该直链。
- 运营闭环:账号授权 + 版本开关 + 封禁 / 停用 + 按用户审计,全部在后台「固件登记簿」完成。

## 二、文档地图

| 文档 | 看什么 | 入口 |
| --- | --- | --- |
| **项目总览** | 功能页面、技术栈、构建 / 发布 / 部署、商业模式 | [../README.md](../README.md) |
| **当前项目架构** | 当前 Tauri/Rust 分层、前端、IPC、安全边界、服务端、测试和发布 | [project-architecture.md](project-architecture.md) |
| **历史架构文档** | WPF 历史实现、Worker / D1 背景和迁移前设计记录 | [architecture.md](architecture.md) |
| **Tauri/Rust 迁移架构快照** | 当前 `5.3codex` 迁移阶段的 crate 分层、命令/event 边界与验收口径 | [architecture-tauri-migration.md](architecture-tauri-migration.md) |
| **客户端 Rust/Tauri 架构** | 随 `src/Nwflash.Desktop` 源码分发的 workspace、IPC、资源和构建说明 | [../src/Nwflash.Desktop/docs/rust-tauri-architecture.md](../src/Nwflash.Desktop/docs/rust-tauri-architecture.md) |
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
├─ src/Nwflash.Desktop/      # Tauri/Rust 桌面客户端(React + Rust workspace)
│  ├─ src/                    # React 页面与路径安全 IPC DTO
│  └─ src-tauri/crates/       # domain/application/infrastructure/windows/tauri
├─ cloudflare/               # 后端(TypeScript Worker + D1)
│  ├─ src/index.ts           # api.nwflash.cc.cd · Worker nwflash-rom(登录/版本/ROM/心跳/在线)
│  ├─ web/src/index.ts       # web.nwflash.cc.cd · Worker nwflash-web(API + 托管 SPA)
│  ├─ web/src/admin.html     # 「固件登记簿」后台单页(版本/用户/日志/在线/使用日志五菜单)
│  ├─ user/src/index.ts      # user.nwflash.cc.cd · Worker nwflash-user(用户自助 API)
│  ├─ user/src/user.html     # 「我的账户」用户门户(我的日志/在线会话/修改密码)
│  ├─ website/src/index.html # nwflash.cc.cd 官网(高级白液态玻璃单页)
│  ├─ website/src/index.ts   # nwflash.cc.cd · Worker nwflash-site(托管官网)
│  └─ wrangler.toml          # D1 绑定 + 自定义域 + vars + Cron
├─ tests/VivoKsu.App.Tests/  # 桌面应用单元测试(397 用例)
├─ scripts/                  # Publish-Release.ps1 / Upload-Resources.ps1 / verify-*.ps1
└─ docs/                     # 本文档 + architecture.md + safeflash-ota.md
```

### 当前 Tauri/Rust 边界

> 2026-08-21 产品决策：运行时镜像/工件哈希门禁与跨步骤手机 serial 绑定已从当前业务流程移除；发行物和受控资源的完整性校验保留。完整规则见 [product-decisions.md](product-decisions.md)。

- Bearer token 只保存在 Rust `AppState.session_token`；登录返回的 `AuthSessionDto` 与 TypeScript session payload 只含身份信息。
- 公开 handler 不接受原始 Quick Flash plan/命令数组、未经 HTTP(S) 校验的任意 ROM/固件 URL 或 Rust 资源路径。固件提取支持受 HTTP(S)、Range 读取和 opaque ID 约束的远程命令；payload URL 直接交给支持 Range 的提取工具，按所选分区读取；本地固件检查/提取、受约束的 Safe Flash 与 Quick Flash 工作流仍可用。
- 产品每次启动只作用于当前发现的一台设备；同时发现多台设备会被拒绝。`DeviceSnapshot` 和 TypeScript `DeviceSnapshotPayload` 包含 serial 供界面显示，但浏览器不能提交、选择或伪造执行 serial。Rust 在每个执行边界从当前唯一设备派生 ADB/Fastboot 命令目标；Quick Flash 会在构造 flash、切槽和重启命令前用该 serial 覆盖计划中的瞬态预览值，不因预检与执行期间 serial 改变而拒绝。
- scrcpy Windows archive 使用固定的 Genymobile v4.1 官方直链、`11,305,298` 字节和固定 SHA-256，下载器同时校验长度与摘要，发布 payload 后才清 staging；不调用 `releases/latest` API，也不接受用户路径或 PATH 回退。ROOT 管理器在 bundle/cache 候选中验证非空、可选 SHA-256 和 APK 结构；payload_dumper readiness 校验预期 SHA-256，损坏 cache 会先删除再尝试校验重装。
- 进程 stdout/stderr 在子进程运行期间由独立 reader 并发排空；正常完成会在构造输出前回收 reader，取消或超时会在终止并回收子进程后回收 reader。大输出与 reader 失败回归测试覆盖该边界。ROOT 镜像/修补工件保留路径、格式、非空/大小、不透明 ID 和 session epoch 约束，不再使用运行时 byte fingerprint 或 SHA-256 拒绝后续消费。

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
| 改 Tauri/Rust 页面与 IPC | `src/Nwflash.Desktop/src/` + `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/` |
| 跑 Tauri/Rust 测试 | `cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --workspace` |
| 构建 Tauri 前端/桌面端 | `npm --prefix src/Nwflash.Desktop run build` + `cargo build --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml` |
| 验证 Tauri 发布物 | `scripts/Publish-TauriRelease.ps1` · `scripts/Verify-TauriRelease.ps1` · `scripts/Test-TauriRelease.ps1` |
| 跑桌面测试 | `dotnet test tests/VivoKsu.App.Tests/…csproj` |
| 发布桌面端 | `scripts/Publish-Release.ps1` → `artifacts/release/` |
| 部署 Worker | `cd cloudflare && npx wrangler deploy`(web / user 子目录同理) |

## 六、WPF 历史基线(2026-08-15)

- **后端**:全部 Cloudflare;旧自建 .NET 服务端已删除;api / web / user / website 均已部署。
- **后台**:「固件登记簿」控制台(五菜单 + 服务健康带 + № 登记册 + 撕口 token 凭证 + OKAY/FAIL 协议回显)已上线。
- **WPF 桌面端（历史快照）**:397 测试全绿;登录门禁 + 强制登录 + 封禁 / 版本控制接线完整。
- **WPF 发布瘦身（历史快照）**:framework-dependent + 原生 AOT 引导器(`VivoKsu.Launcher.exe`,.NET 缺失→微软直链静默装);scrcpy / ROOT 管理器 APK / payload_dumper 外置按需下载(GitHub Release + gh-proxy.com/ghfast.top/ghproxy.net 多镜像 failover + SHA-256 校验);驱动 / adb-fastboot / magiskboot 保留随包。解压 ~205MB → ~24MB,zip ~95MB → ~11MB。详情见 [architecture.md §9](architecture.md)。
- **界面整合(2026-08-15)**:左侧菜单按刷机链路分组;左下角账号/时间/登出栏(同进程登出回登录窗,刷写中登出禁用);全部主进度统一到右上角「操作进度」区;文件管理传出弹保存位置对话框。设计文档见 [设计规格](superpowers/specs/2026-08-15-ui-consolidation-design.md) / [实施计划](superpowers/plans/2026-08-15-ui-consolidation.md)。
- **在线会话**:客户端每 5s 心跳保持在线;后台「在线状态」实时查看会话(用户/版本/IP/时长)并**强制下线**(≤5s 内客户端退出);客户端「在线状态」页查看在线用户与时长;心跳 force_exit / 封禁 / 426 均走防变砖退出(刷写中先取消、等 Idle 再退)。
- **操作门禁 + 使用日志**:客户端每个用户操作运行前经服务端 `POST /api/operation/authorize` 许可(默认放行、封禁/停用拒绝);执行后批量上传使用日志,后台「使用日志」按操作分类查看。
- **软件菜单 + 驱动安装**:「软件」页展示 Nwflash 版本 / USB 驱动 / scrcpy / payload_dumper 就绪状态;启动检测到未装 vivo USB 驱动时弹窗提醒,一键以管理员权限静默安装(pnputil 通配符递归装 ADB / fastboot / 联发科驱动 + 写 adb_usb.ini)。
- **用户门户**:`user.nwflash.cc.cd` 客户自助后台(高级白 + 毛玻璃)——我的查询日志 / 在线会话(⟠ 强制下线)/ 修改密码,已上线。
- **官网**:`nwflash.cc.cd` 官网(高级白 + 液态玻璃 + 高斯模糊)单页已上线,含产品介绍 / 功能 / 更新日志。

当前 Tauri/Rust 实现与未完成项以 [迁移架构快照](architecture-tauri-migration.md) 为准；上面的 WPF 条目保留作历史基线，不代表迁移后的 IPC/resource boundary。
