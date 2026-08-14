# VivoKsu

Vivo 手机刷机 / Root 工具箱 —— **商业付费工具**,Windows WPF 桌面应用(.NET 8)。

提供 ADB / Fastboot 设备检测、分区可视刷写、快速刷写(KernelSU)、payload 解包与云端提取、ADB 投屏、文件管理等能力,全程中文界面、teal 主题。

## 项目定位与商业模式

VivoKsu 是**商业付费工具**:

- **登录授权**:桌面端启动必须用后台创建的账号登录,未登录不可进入主界面(登录门禁)。账号由 `web.nwflash.cc.cd` 后台「用户管理」创建。
- **服务端全在 Cloudflare,零自有服务器**:API `api.nwflash.cc.cd`(Worker `nwflash-rom`)+ 后台 `web.nwflash.cc.cd`(Worker `nwflash-web`)+ 数据库 D1 `nwflash-db`,认证、版本授权、审计、后台管理全在 Cloudflare Edge。
- **商业模式 = 账号授权制**:用户登录即可使用,**不对用户按次扣点 / 限制次数**。拿 ROM 的链路是 `api.nwflash.cc.cd` 从上游 VOTA 取链接返回给工具,中间不涉及对用户账号的任何扣点计费。
- **上游信用点 = 运营方成本**:VOTA 的信用点扣的是 Worker 所持 token 账户(运营方),由开发者承担,不进客户端、不向用户收。
- **授权与控制**:版本在后台「版本号控制」启用才可查(未启用 → 404);封禁 / 停用即时生效(登录 401 / 查询 403);每次查询按用户记入访问日志。

## 功能页面

| 页面 | 说明 |
| --- | --- |
| **设备概览** | ADB / Fastboot 设备自动检测,显示串号、型号、系统版本、电量,一键刷新设备信息 |
| **快速刷写** | 预设分区表(boot / init_boot / vendor_boot / lk)选择镜像并刷写;等待 Fastboot 设备、自动重启、双槽双刷 |
| **ADB 投屏** | 通过 scrcpy 管理 Android 屏幕镜像(自动推送 scrcpy-server) |
| **Vivo ROOT** | Root 自动化:KernelSU 安装、vendor_boot 补丁处理(自动处理官方与 GKI 内核)、magiskboot 操作、Root 资源管理 |
| **文件管理** | ADB Root 通道的文件浏览 / 上传 / 下载 / 删除 |
| **可视刷写** | 读取全量分区表,卡片式列表勾选后依序执行**备份 / 写入 / 擦除**;支持 Fastboot 与 ADB Root 双通道 |
| **固件提取** | 解包 `payload.bin` / OTA zip;或粘贴云端直链,通过 HTTP Range **按需下载**镜像(不下载整个包),带实时进度与速度 |
| **VIVO 线刷** | 一键刷机:adb 读设备 PD/版本 → 查 `api.nwflash.cc.cd` 拿 OTA 链接 → 多分片下载 → 解压解包 payload → **跳过 preloader/lk** → 自动重启 fastbootd 逐个刷入其余分区 → 重启。也可选择本地 .zip / payload.bin 走同流程 |
| **操作日志** | 按级别(信息 / 成功 / 警告 / 错误)记录所有操作,`[HH:mm:ss]` 时间戳 + 消息的刷机工具式单行显示 |

## 技术栈

- **.NET 8**(`net8.0-windows`)、**WPF**、MVVM
- **CommunityToolkit.Mvvm** 8.4 —— `[ObservableProperty]` / `[RelayCommand]`
- **HandyControl** 3.5.1 —— UI 控件库
- **SharpCompress** 0.37.2、**ZstdSharp.Port** 0.8.1 —— 压缩 / zstd 解压
- **xunit** + **FluentAssertions** —— 单元测试(当前 **253** 个应用用例 + **8** 个服务端用例全绿)

## 目录结构

```
VivoKsu 工具/
├─ VivoKsu.slnx                     # 解决方案
├─ src/
│  ├─ VivoKsu.App/
│  │  ├─ Models/                    # 领域模型(AppPage、分区、payload、设备快照…)
│  │  ├─ Services/                  # 业务服务与基础设施
│  │  ├─ ViewModels/                # 各页面 MVVM 视图模型
│  │  ├─ MainWindow.xaml            # 单窗口多页面导航与全部 XAML
│  │  ├─ apk/                       # KernelSU 安装包(KSU.APK / KernelSU.apk)
│  │  ├─ payload-tools/             # payload_dumper.exe(payload-dumper-rust)
│  │  ├─ platform-tools/            # adb / fastboot
│  │  ├─ root-tools/                # magiskboot.so
│  │  └─ scrcpy/                    # scrcpy(发布时由脚本自动补齐)
├─ cloudflare/                      # Cloudflare Worker:VivoKsu ROM 代理(api.nwflash.cc.cd)
├─ tests/
│  └─ VivoKsu.App.Tests/            # 桌面应用单元测试
├─ scripts/
│  ├─ Publish-Release.ps1           # 一键发布 self-contained 版本
│  ├─ Ensure-Scrcpy.ps1             # 发布前自动获取 scrcpy
│  └─ verify-*.ps1                  # UI 自动化验证脚本(启动→UIA 导航→截图)
├─ docs/superpowers/                # 设计与计划文档
└─ third_party/                     # fastboot-rs 源码参考
```

## 架构与关键设计

### 组合根与依赖注入

无第三方 DI 容器,`Services/AppComposition.cs` 手写组合根:构建后端 → 会话 → 协调器 → 各页面 VM → 组装 `MainViewModel`,并注册跨页面回调(固件提取 / Root 产物一键映射到快速刷写)。

### 设备监视(DeviceMonitorService)

后台 `PeriodicTimer`(默认 3 秒)轮询设备状态。**关键设计**:心跳轮询只在设备身份(连接状态 / 串号)发生变化时才触发下游 `DeviceRefreshed`。**可视刷写的分区表只在用户点击「读取分区表」时读取**——设备接入 / 断开 / 切换、操作完成后的补偿刷新都不会触发重读,避免分区表被反复读取打断操作;`DeviceRefreshed` 仅用于设备概览 / 镜像协调等与分区表无关的更新。

### 分区传输抽象

`IPartitionTransport` 封装两种通道,可视刷写按连接状态自动选择:

- `FastbootPartitionTransport` —— fastboot `getvar` / `flash` / `erase`
- `AdbRootPartitionTransport` —— adb root + `dd` 读写,失败时杀进程树并合并 stderr

`PartitionExecutionService` 依序执行每个选中分区,经 `OperationCoordinator` 上报进度(100ms 节流)。

### 固件提取

格式检测(`FirmwareFormatDetector`)按魔数分流:

- **标准 OTA zip**(`PK` 魔数)→ `payload_dumper`(内置二进制)走 **HTTP Range 只读所需 blob**,远程源不落地整包
- **Vivo 专用格式**(`1f8b` gzip)→ `VivoFirmwareExtractor` **流式解压 gzip→tar**,直接列出 / 提取分区镜像

**实时进度(重点)**:payload_dumper 不输出流式进度,且其网络读取(Rust reqwest 走 IOCP/AFD)不计入进程 `ReadTransferCount`;实际验证可靠信号是**进程写入字节数 `WriteTransferCount`** —— 后台每 200ms 采样 `GetProcessIoCounters`,按分区 `size_in_bytes` 作分母,得到真实连续的进度条与速度。Vivo gzip 路径则以已解压字节 / gzip 总量直接报连续进度。

### VIVO 线刷(安全刷写)

一键刷机链路(详见 [docs/safeflash-ota.md](docs/safeflash-ota.md)):

- **OTA 下载** `OtaDownloadService`:bezzad/Downloader 多分片,含 1 字节 bug / 失败假成功 / 磁盘预检等修复;staging 优先系统 SSD。
- **解压解包** `FirmwarePartitionExtractor`:自动分流 payload OTA(PD2417)/ 直接镜像 zip(PD2057)/ firmware-update 镜像,过滤 `preloader*` 与 `lk`。
- **刷写** `FastbootCliRunner`:调唯一 `platform-tools/fastboot.exe`(35.0.2-eng,带连续进度 + 可读错误),`adb reboot fastboot` 进 fastbootd,`getvar partition-type` 预检跳过设备缺失分区,逐个 flash 后 `fastboot reboot`。
- 操作日志按 `[HH:mm:ss] 消息` 单行等宽显示刷机进度,自动滚动。

### UI 现代化

参考 taste-skill 审美原则迭代:统一 teal 配色、圆角卡片分区列表(固件提取与可视刷写同款)、表单式页面头部、双进度条底栏(当前分区 + 总进度百分比 + 速度 / 耗时)。

## 服务端(Cloudflare —— api.nwflash.cc.cd / web.nwflash.cc.cd)

**整个后端全部托管在 Cloudflare,无自有服务器**:API(Worker `nwflash-rom`)、后台管理(Worker,`web.nwflash.cc.cd`)、数据库(D1 `nwflash-db`)都在 Cloudflare Edge。桌面应用只连 `api.nwflash.cc.cd`;上游 [VOTA API](https://api.otau.cc.cd) 完全不动。

| 端点 | 说明 |
| --- | --- |
| `GET /health` | 健康检查,返回 `{status, source}` |
| `GET /api/rom?pd=PD2417&version=16.2.12.0.W10.V000L1` | 按 PD + 版本号返回 OTA 下载链接 |

**凭据隔离**:VOTA API Token 以 Worker 机密(`wrangler secret put VOTA_API_TOKEN`)存在 `api.nwflash.cc.cd` 上,**不进入 VivoKsu 桌面端**。VivoKsu 代码里没有任何 `api.otau.cc.cd` / token 信息,只连 `api.nwflash.cc.cd`。

**计费**:上游 VOTA 的信用点由**运营方**(Worker 所持 token 账户)承担,**不对用户扣点计费** —— 用户登录即可查询,不限制次数。

**代码**:`cloudflare/`(TypeScript Worker + wrangler.toml),worker 名 `nwflash-rom`。非机密项在 `wrangler.toml [vars]`:`VOTA_BASE_URL`(默认 `https://api.otau.cc.cd`)、`VOTA_ACTION`(`resolve_url` OTA / `resolve_flash_url` 线刷)、`VOTA_VER`(`0.1.0`)。

**部署**:

```bash
cd cloudflare
npm install
npx wrangler login                    # 浏览器登录 Cloudflare 账户
npx wrangler secret put VOTA_API_TOKEN   # 粘贴 VOTA 的 API Token(机密,不进代码)
npx wrangler deploy                   # 部署并绑定自定义域 api.nwflash.cc.cd
```

**错误映射**:`NOT_FOUND`/`not found`→404、`AUTH_FAIL`→401、`INSUFFICIENT_CREDITS`→402、`FORBIDDEN`→403、`RATE_LIMITED`→429、其它→502。

**已实现**:登录系统(桌面端门禁 + `/api/login`)、后台管理(`web.nwflash.cc.cd`)、按用户审计与封禁,均已在 Cloudflare 上。**商业模型**:账号授权制 —— 登录即用,不对用户按次扣点 / 限制次数;上游 VOTA 信用点为运营方成本。

> 早期自建 .NET 服务端(`src/VivoKsu.Server/`)已整体删除 —— 线上后端 100% 跑在 Cloudflare Workers(仅支持 JavaScript/TypeScript)+ D1,桌面端直连 `api.nwflash.cc.cd`;VOTA 凭据只存在 Worker 机密里,不再有自托管代码。

## 构建与测试

```bash
# 构建(Debug)
dotnet build src/VivoKsu.App/VivoKsu.App.csproj -c Debug

# 运行全部测试
dotnet test tests/VivoKsu.App.Tests/VivoKsu.App.Tests.csproj -c Debug

# 运行应用
./src/VivoKsu.App/bin/Debug/net8.0-windows/VivoKsu.App.exe
```

## 发布

```powershell
./scripts/Publish-Release.ps1
```

产出 **self-contained** `win-x64` 版本:

- `artifacts/release/VivoKsu-win-x64/` —— 可分发目录
- `artifacts/release/VivoKsu-win-x64.zip` + `.sha256`
- `SHA256SUMS.txt` —— 目录内每个文件的 SHA-256 清单

发布前 `Ensure-Scrcpy.ps1` 自动补齐 scrcpy,并清理废弃资源(Sukisu.APK、ksud)。

## 内置组件

| 组件 | 来源 | 用途 |
| --- | --- | --- |
| `payload-tools/payload_dumper.exe` | payload-dumper-rust | OTA payload 解包、云提取(本地 / zip / URL) |
| `platform-tools/` | Android SDK Platform Tools | adb、fastboot |
| `scrcpy/` | scrcpy | 屏幕镜像(发布时自动获取) |
| `root-tools/magiskboot.so` | Magisk | vendor_boot 补丁处理 |
| `apk/KSU.APK`、`apk/KernelSU.apk` | KernelSU | Root 管理器安装包 |

## 已知限制

- **payload 分区内部百分比无法测量**:payload_dumper 预分配输出文件且不流式输出进度,分区内的进度条按进程写入字节驱动(真实但以分区为单位),分区内更细的百分比受工具二进制限制无法获得。
- **分区操作有真实设备风险**:写入 / 擦除会修改设备分区,执行前有确认弹窗,任务在首个失败处分区停止。
- **脚本编码**:发布 / 验证用的 `.ps1` 必须保持纯 ASCII(本机无 BOM 的 UTF-8 脚本被按 GBK 读取会乱码)。
- **唯一 fastboot.exe 待真机验证**:fastboot 35.0.2-eng 在 vivo fastbootd 逐个刷分区是唯一未真机实测环节。
- **下载盘需 ~25GB 空闲且最好是 SSD**:bezzad 多分片随机写在 HDD 会停滞(staging 自动优先系统盘)。

## 相关文档

> 📚 **项目索引**:[docs/index.md](docs/index.md) —— 所有文档 / 代码 / 服务 / 数据的导航地图,从此出发。

- [docs/architecture.md](docs/architecture.md) —— **项目架构文档**(系统总览 / 桌面端模块 / Worker / D1 / 数据流 / 设计决策)。
- [docs/safeflash-ota.md](docs/safeflash-ota.md) —— VIVO 线刷(安全刷写)流程、OTA 格式、下载/刷写内部细节与踩坑。
- [cloudflare/API.md](cloudflare/API.md) —— **api.nwflash.cc.cd 接口契约**(端点、参数、响应、错误码、计费、功能记录)。
- [cloudflare/README.md](cloudflare/README.md) —— Cloudflare Worker(api.nwflash.cc.cd)部署说明。
- [cloudflare/web/README.md](cloudflare/web/README.md) —— **web.nwflash.cc.cd 后台管理**(登录/版本控制/用户/日志/安全)。
