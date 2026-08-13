# VivoKsu

Vivo 手机刷机 / Root 工具箱 —— Windows WPF 桌面应用(.NET 8)。

提供 ADB / Fastboot 设备检测、分区可视刷写、快速刷写(KernelSU)、payload 解包与云端提取、ADB 投屏、文件管理等能力,全程中文界面、teal 主题。

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
| **线刷准备** | 读设备 PD/版本 → 查 `VivoKsu.Server` 拿 OTA 链接 → **多分片并行下载**(bezzad/Downloader)→ 解压出 Preload.bin(解包分区镜像为后续阶段) |
| **操作日志** | 按级别(信息 / 成功 / 警告 / 错误)记录所有操作 |

## 技术栈

- **.NET 8**(`net8.0-windows`)、**WPF**、MVVM
- **CommunityToolkit.Mvvm** 8.4 —— `[ObservableProperty]` / `[RelayCommand]`
- **HandyControl** 3.5.1 —— UI 控件库
- **SharpCompress** 0.37.2、**ZstdSharp.Port** 0.8.1 —— 压缩 / zstd 解压
- **xunit** + **FluentAssertions** —— 单元测试(当前 **217** 个用例全绿)

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
│  └─ VivoKsu.Server/               # 独立 Web 服务:从 VOTA API 获取 OTA 链接
├─ tests/
│  ├─ VivoKsu.App.Tests/            # 桌面应用单元测试
│  └─ VivoKsu.Server.Tests/         # 服务端单元与端到端测试
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

### UI 现代化

参考 taste-skill 审美原则迭代:统一 teal 配色、圆角卡片分区列表(固件提取与可视刷写同款)、表单式页面头部、双进度条底栏(当前分区 + 总进度百分比 + 速度 / 耗时)。

## 服务端(VivoKsu.Server)

独立 ASP.NET Core Web 服务,凭据放在服务端,桌面应用只需用 **PD + 版本号** 查询即可拿到 OTA 下载链接。上游为 [VOTA API](https://api.otau.cc.cd)(HTTPS,POST + JSON)。

| 端点 | 说明 |
| --- | --- |
| `GET /health` | 健康检查,返回当前数据源(真实 / 演示) |
| `GET /api/rom?pd=PD2417&version=16.2.10.0.W10.V000L1` | 按 PD + 版本号返回 OTA 下载链接 |

默认调用 VOTA `resolve_url`(OTA 全量包,-1 信用点);可在配置里改用 `resolve_flash_url`(线刷包,-3)或 `dev_resolve`(设备端,用 `device_id` 鉴权,无需 user/pass)。VOTA 返回 `ok:false` 时按错误码映射 HTTP 状态(`NOT_FOUND`→404、`INSUFFICIENT_CREDITS`→402、`AUTH_FAIL`→401、`RATE_LIMITED`→429 等)。

**配置**:`src/VivoKsu.Server/appsettings.json` 的 `VotaApi` 段填入凭据后即切到真实数据源;留空时退回演示数据源(返回占位链接),便于先联调客户端。

```json
"VotaApi": {
  "BaseUrl": "https://api.otau.cc.cd",
  "User": "你的用户名",
  "Pass": "你的密码",
  "Ver": "1.0.0",
  "DeviceId": "",
  "Action": "resolve_url"
}
```

**运行**:

```bash
cd src/VivoKsu.Server
dotnet run
```

默认 HTTPS `https://localhost:7243`、HTTP `http://localhost:5143`。测试:`dotnet test tests/VivoKsu.Server.Tests`。

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
