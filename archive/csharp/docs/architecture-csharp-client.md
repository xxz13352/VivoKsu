# VivoKsu C# 客户端架构文档(WPF 归档版)

> **本文范围**:仅描述 `archive/csharp` 下的 WPF 客户端(`VivoKsu.App` / `VivoKsu.Bootstrapper` / `VivoKsu.App.Tests`)。
> 系统整体架构(服务端 / 后台 / Tauri 版)见 [architecture.md](architecture.md) 与 [architecture-tauri-migration.md](architecture-tauri-migration.md)。
> 更新基准:2026-09-06(云端 OTA 提取落地、467 测试全通过、Release 发布流程验证)。

## 1. 定位与边界

- **技术栈**:WPF(.NET 8, `net8.0-windows`)+ CommunityToolkit.Mvvm + HandyControl;设备操作走 fastboot-rs 原生 DLL 与 fastboot.exe CLI 双后端。
- **与服务端的关系**:所有服务端能力(`api.nwflash.cc.cd`,Cloudflare Worker)由 `OtaApiClient` 一族封装,契约以 `cloudflare/API.md` 为准。
- **与 Rust 客户端的关系**:Rust 版(`src/Nwflash.Desktop`)是功能参照与安全基线。C# 版已对齐其全部 API 面(11/12 条路由)与关键业务语义(登录签名租约、心跳序号自愈、日志持久化管道、崩溃补传、完整性遥测、SPKI 钉扎、云端 OTA 提取)。刻意不做:`/api/usage/traces/v2`(依赖 Rust 保护模块的数据源)与 `nwflash-protection` 保护层(防调试/租约验签/VMP)。

## 2. 项目布局

```
archive/csharp/
├─ VivoKsu.slnx                    # 解决方案(App + Bootstrapper + Tests)
├─ src/
│  ├─ VivoKsu.App/                 # 主程序(WPF)
│  │  ├─ App.xaml(.cs)             # 启动生命周期:崩溃钩子/版本门禁/登录循环/退出收尾
│  │  ├─ MainWindow.xaml           # 单窗多页主界面(9 个功能页)
│  │  ├─ Models/                   # 不可变记录(契约模型 + 页面状态快照)
│  │  ├─ Services/                 # 全部业务服务(见 §4)
│  │  ├─ ViewModels/               # 每页一个 VM,组合根装配
│  │  └─ Assets/ platform-tools/ … # 内置资源(adb/fastboot/root 工具)
│  └─ VivoKsu.Bootstrapper/        # NativeAOT 原生启动器(5.5MB 单文件)
│     └─ 编入 VivoKsu.App/Services/DotNetRuntimeDetector.cs(共享链接,保持 AOT 兼容)
├─ tests/VivoKsu.App.Tests/        # xUnit + FluentAssertions + stub HttpMessageHandler
├─ scripts/Publish-Release.ps1     # 发布:framework-dependent + AOT launcher + 语言包瘦身 + SHA256 + zip
└─ docs/                           # 本文档与历史基线
```

## 3. 启动生命周期(`App.xaml.cs`)

```
OnStartup
 ├─ 注册崩溃钩子:DispatcherUnhandledException / AppDomain.UnhandledException
 │   └─ CrashReporter.Write → %LOCALAPPDATA%\VivoKsu\crash.log([epoch] panic: … 行格式)
 ├─ 订阅 ApiTlsPinPolicy.PinRejected → IntegrityReporter( PinValidation/PinMismatch,内置 10s 限频)
 ├─ 后台启动任务 RunStartupServerTasksAsync(延迟 8s,不阻塞启动)
 │   ├─ RefreshPinsetAsync:拉取签名 pinset 补充钉扎名单(失败沿用内置双 pin + 本地缓存)
 │   └─ CrashReporter.UploadPendingAsync:补传上次崩溃 → 成功清空 crash.log(失败保留,匿名可报)
 ├─ BlockForForcedUpdate:GET /api/app/version,force_update → 弹更新窗 + Shutdown(网络失败放行)
 └─ RunApplicationLoop(登录循环)
      登录窗(LoginService)→ 成功 → AppComposition.CreateDefault()
        → StartSessionAsync(token, username, sessionId):
            SetAuthToken → UsageReporter.PublishSession(账号 SHA-256 不透明化)
            → Heartbeat.Start(登录时生成的 sessionId)→ Online.Start → UsageReporter.Start
        → MainWindow
      登出 → StopAsync → 回登录窗;退出 → Shutdown
OnExit(5s 预算): Online.Stop → Heartbeat.StopAsync(goodbye) → Monitor.StopAsync
      → UsageReporter.Stop + CloseSessionAsync(5s)→ 临时文件清理
```

## 4. 服务分层(按域)

### 4.1 服务端 API 面(11/12 条路由)

| 路由 | C# 封装 | 说明 |
| --- | --- | --- |
| `POST /api/login` | `LoginService.LoginAsync` | 必带 `client_version/build_id/process_nonce/session_id`(签名租约契约);`LoginResult` 回传 sessionId |
| `GET /api/me` | `LoginService.ValidateTokenAsync` | 记住登录校验 |
| `POST /api/heartbeat` | `OtaApiClient.HeartbeatAsync` | 活动心跳带完整绑定 + 递增 `sequence`;goodbye 只带 session_id;解析 `lease_payload` 回同步序号 |
| `GET /api/online` | `OtaApiClient.GetOnlineAsync` | 在线列表(仅显示名/版本/时长) |
| `POST /api/operation/authorize` | `OtaApiClient.AuthorizeOperationAsync` | 操作许可门禁(ServerOperationGate 包装,fails-open on 5xx) |
| `POST /api/usage/logs` | `OtaApiClient.UploadUsageLogsAsync` | 批量 ≤100;`UsageLogUploadResult{Ok,Received}` |
| `POST /api/integrity/report` | `OtaApiClient.ReportIntegrityAsync` | 本地前置校验(闭集字段/RFC 标识符/Unix 秒);`IntegrityReporter` 接线心跳 409/401、钉扎失败 |
| `POST /api/diagnostics/crash` | `OtaApiClient.UploadCrashReportAsync` | 202/200 均为成功;`CrashReporter` 接线启动补传 |
| `GET /api/security/pins` | `OtaApiClient.RefreshPinsetAsync` | pinset 载荷校验 + 本地缓存(`ApiTlsPinPolicy`) |
| `GET /api/app/version` | `AppVersionService.CheckAsync` | 启动强制更新门禁 |
| `GET /api/rom` | `OtaApiClient.ResolveAsync` | ROM 解析(云端 OTA 提取与安全刷写共用) |
| `GET /health` | `OtaApiClient.CheckHealthAsync` | 连通性诊断 |
| ~~`POST /api/usage/traces/v2`~~ | — | 刻意不做(见 §1) |

### 4.2 会话与在线(登录态核心)

- `ClientSession`(静态):`BuildId = "vivoksu-<版本>"`(进程内恒定)、`ProcessNonce`(每次启动随机)、`NewSessionId()`(每次登录)。服务端租约绑定四元组全来自这里。
- `HeartbeatService`:5s 周期 + 每请求 10s 超时,严格串行(PeriodicTimer,在途不发新请求)。**序号自愈**:首次 409 视为「服务端已推进但响应丢失」,序号 +1 重试一次;自愈后仍 409 / 401 / 403 → 强制回登录。成功后优先采信响应租约载荷里的序号。
- `OnlineViewModel`:在线状态轮询展示。

### 4.3 使用日志管道(对齐 Rust usage_reporter)

- `UsageLogEntry`(+ `UsageLogDetail` 过程明细):snake_case 契约,`details` 空时整个字段不序列化。
- `UsageLogSpool`:**先落盘后上传**(原子写 tmp → flush(true) → Move overwrite),上传成功才移除;`UsageLogOwner` 用 SHA-256 派生不透明账号(算法与 Rust 一致),磁盘上永不出现明文账号。
- `UsageLogUploader`:排队绑 owner → 30s 定时 / 20 条阈值 → **排空循环**(批量 100)→ 4xx 整批丢弃(防毒丸)、其余保留重试 → `FlushAsync(budget)` 退出预算 → `Stop` 后丢弃新记录。
- `OperationCoordinator.RecordUsage`:每次操作完成上报,`details` 取 `OperationLogService.SnapshotFor(operationId)`(≤500 条、单条 ≤16384)。
- 生产队列:`%LOCALAPPDATA%\VivoKsu\usage-logs.json`(跨进程续传)。

### 4.4 诊断三件套

- `CrashReporter`:异常 → crash.log(`[epoch] panic:`);价值过滤(私钥块 fail-closed 整条拒、`token=/password=` 替换占位、路径保留);启动延迟 8s 补传,event_id = `crash-{epoch}-{len}`(与 Rust 同规则)。
- `IntegrityReporter`:best-effort 上报,客户端 10s 限频(服务端 60s/20 条窗口 + event_id 幂等)。接线点:心跳 409 自愈(`SequenceRollback`)、401/403/终局 409(`LeaseExpired`/`LeaseBindingInvalid`)、TLS 钉扎失败(`PinValidation/PinMismatch`)。

### 4.5 TLS 钉扎(`ApiTlsPinPolicy`)

- 仅对 `api.nwflash.cc.cd` 生效:证书 SPKI SHA-256 必须命中**内置双 pin**(与 Rust 常量逐字一致)或本地 pinset 缓存;localhost 自签放行;`AllowAutoRedirect=false` + 30s 超时统一在 `CreateHandler()`。
- `OtaApiClient` / `LoginService` / `AppVersionService` / `RootOtaCloudExtractService` 默认 HttpClient 全部走该策略。
- 取舍:pinset 是 Ed25519 签名信封,.NET 8 无内置 Ed25519 → 载荷只做 host/有效期/pin 格式校验,签名验签缺失由「内置 pin 始终在名单」兜底。

### 4.6 云端 OTA 提取(对齐 Rust root_ota + remote_firmware)

- `RemoteRangeStream`:HTTP Range 随机访问流(512KB 块缓存、206 强校验),不支持 Range 的服务器显式失败。
- `RemoteZipClient`:**自研 ZIP 解析**(EOCD → zip64 locator/EOCD → 中央目录 → zip64 extra 0x0001 → local header)+ 解压后**显式 CRC-32 与长度校验** + `.partial` 原子改名。
  > ⚠️ **为什么不用 .NET ZipArchive**:实测 .NET 8 Read 模式不校验 CRC(损坏数据/篡改 CRC 记录均静默解压成功)。刷机工具不可接受,必须显式校验。
- `RootOtaCloudExtractService`:读设备版本(`ro.build.version.bbk` 权威串,`VivoVersionParser` 共享解析)→ ResolveAsync(URL 只留服务内存,不进日志/UI)→ 按需提取 `init_boot`(优先)/`boot` + `vendor_boot` → 灌入 `RootViewModel`,修补/刷写与手选镜像共用同一条链路。

### 4.7 设备操作

- `FastbootRsBackend`(fastboot-rs 原生 DLL)+ `FastbootCliRunner`(唯一 fastboot.exe):双后端;分区读写/擦除/备份、quick-flash 预设、safe-flash 槽位、线刷包检查与提取、ROOT 修补(VivoKsuDevicePatchService / VivoVendorBootProcessor)、镜像提取(payload dumper)。
- `OperationCoordinator`:操作串行门禁 + 许可检查 + 使用日志记录 + 取消传播。

## 5. 测试

- `tests/VivoKsu.App.Tests`:**467 个测试**,xUnit + FluentAssertions。
- HTTP 交互全部通过注入 `HttpMessageHandler` 桩;远程 ZIP 测试用**内存构造的真实 ZIP + 本地 HttpListener Range 服务器**(206/Content-Range/实际字节数统计)验证「只下载所需字节」。
- 心跳自愈、日志管道持久化/账号隔离、崩溃解析/过滤、钉扎 pin 派生(自签证书独立复算 SPKI)均有专项回归。

## 6. 构建与发布

```
# 全量测试
dotnet test tests/VivoKsu.App.Tests/VivoKsu.App.Tests.csproj -c Release

# 正式发布(等价 scripts/Publish-Release.ps1)
dotnet publish src/VivoKsu.App/VivoKsu.App.csproj -c Release -r win-x64 --self-contained false
dotnet publish src/VivoKsu.Bootstrapper/VivoKsu.Bootstrapper.csproj -c Release -r win-x64   # NativeAOT
# → artifacts/release/VivoKsu-win-x64/ (+ VivoKsu.Launcher.exe + SHA256SUMS.txt)
# → artifacts/release/VivoKsu-win-x64.zip (+ .sha256)
```

- 发布形态:framework-dependent 主程序 + **NativeAOT 原生启动器**(首次运行检测 .NET 8 Desktop Runtime,缺失则静默下载安装)。
- 语言包瘦身:仅保留 `zh-Hans` / `zh-Hant`。

## 7. 已知坑(构建环境)

| 坑 | 现象 | 解法 |
| --- | --- | --- |
| Git Bash 缺 Windows 环境变量 | NuGet `path1` null 崩溃 | `env APPDATA=… 'ProgramFiles(x86)=…' dotnet …`(变量名带括号必须用 `env`,不能 `VAR=x cmd`) |
| AOT 误报 Cross-OS | `Cross-OS native compilation is not supported` | MSBuild `$(OS)` 读环境变量 `OS`;补 `OS=Windows_NT` |
| AOT 链接器找不到 SDK 库 | `LNK1181: advapi32.lib` | 补 `LIB=<SDK>\um\x64;<SDK>\ucrt\x64`(vcvarsall 在受限环境跑不完) |
| 增量残留跳过 AOT | 产物是托管 dll 而非原生 exe | 重发布前删 `obj/` |
| `.ps1` 非注释编码 | GBK 误读致解析错乱 | `scripts/*.ps1` 保持 ASCII-only(仅英文注释) |

## 8. 设计决策速查

1. **一致性优先**:所有契约细节(snake_case 字段、event_id 规则、owner 派生算法、崩溃行格式)与 Rust 版逐字对齐,服务端一套校验两端通吃。
2. **失败显式化**:钉扎失败/不支持 Range/CRC 损坏/409 终局全部显式报错或上报,绝不静默退化 —— 静默是刷机工具的敌人。
3. **best-effort 边界**:遥测/日志/崩溃补传永不阻塞业务;心跳 goodbye / 日志 flush 有严格退出预算。
4. **先落盘后网络**:一切待上传数据先持久化,网络失败只影响时机不影响数据。
