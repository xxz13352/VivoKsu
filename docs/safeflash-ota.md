# 安全刷写与 OTA 下载 —— 技术细节

> 对应桌面应用「安全刷写」页(替代原「线刷准备」)。本文记录刷机链路各环节的实现细节与踩过的坑。

## 总体流程

```
[设备 adb] --读 ro.product.device + ro.build.version.bbk--> [安全刷写]
   [OtaApiClient] --查 api.nwflash.cc.cd/api/rom--> OTA URL
   [OtaDownloadService] --多分片下载(bezzad/Downloader)--> OTA zip
   [FirmwarePartitionExtractor] --解压解包--> 分区镜像
   --过滤 preloader*/lk--> 待刷分区
   --内联确认-->
   [FastbootRsCliRunner] --adb reboot fastboot--> fastbootd
   --getvar partition-type 探存在性--> 逐个 fastboot flash--> fastboot reboot
```

页面只有两个主操作按钮:**「下载+刷入」**(adb 读设备 → 下载 → 解包 → 刷)与 **「选择固件」**(选本地 `.zip` / `payload.bin`,走同样解包→刷流程)。刷写前有内联确认面板。

## 设备版本读取

优先 `ro.build.version.bbk`(权威,含完整版本号),形如 `DPD2221B_A_16.2.12.0.W10.V000L1`:

```csharp
ParseBbkVersion("DPD2221B_A_16.2.12.0.W10.V000L1")
// => (Codename: "DPD2221B", Version: "16.2.12.0.W10.V000L1")
// 第一段 = 设备代号,最后一段 = 完整版本号(按 _ 拆分)
```

PD 码用 `ro.product.device`(= `Details.Codename`)。bbk 为空 / 版本是通用值(`release-keys`/`unknown`/`not found`)时,回退 `ro.build.display.id` → `ro.build.version.incremental` → `ro.vivo.os.build.display.id`。

> ⚠️ 平台版本键必须与设备 bbk 一致。若设备报旧版(如平台只有 `16.2.12.0`,设备 bbk 报 `15.2.12.0`),查询返回 `record not found`。

## OTA 格式(三种,`FirmwarePartitionExtractor` 自动分流)

`FirmwareFormatDetector` 按魔数判断 `PayloadZip`(PK / CrAU)或 `VivoGzip`(1f8b);PK 时再查 zip 内是否含 `payload.bin` 决定走哪条解包路径:

| 格式 | 典型设备 | 结构 | 解包方式 |
| --- | --- | --- | --- |
| **AOSP payload OTA** | PD2417(MTK) | zip 内 `payload.bin` + `oem_zip/*.zip` + 元数据 | `payload_dumper.exe` 直接解出全部分区(含 system/vendor/product,无需重建)。实测 38 分区,含 `lk` 与 `preloader_raw` |
| **直接镜像 zip** | PD2057(MTK) | zip 根直接是 `*.img` / `*.bin`(preloader*.img/lk/boot/dtbo/vbmeta/tee/gz…)+ `scatter.txt` | `ZipFile` 解出 `*.img`/`*.bin`;system/vendor/product 是块式(`.new.dat`+`transfer.list`),暂不支持 |
| **firmware-update 镜像** | PD2196(Qualcomm) | `firmware-update/*.img` 可直接刷;OS 分区同样块式 | 同上直接解出 |

**过滤规则**:刷入前剔除 `name == "lk"` 或 `name.Contains("preloader")` 的分区(PD2417 实际 38 → 36 个)。其余全刷。

## OTA 下载(bezzad/Downloader 5.9.5)

封装在 `OtaDownloadService`。踩过的坑:

1. **1 字节 bug**:`RangeDownload=true` 若不显式设 `RangeHigh`,库的 `SetRangedSizes` 把总大小算成 `RangeHigh-RangeLow+1 = 0-0+1 = 1`,只下 1 字节。**修法**:先用 `RemoteFileResolver.GetFileInfoAsync` 探测真实大小,设 `RangeLow=0、RangeHigh=总大小-1`。
2. **失败假成功**:库的失败只投递到 `DownloadFileCompleted` 事件(不抛给调用方),`DownloadFileTaskAsync` 正常返回 → 应用误报"下载完成"。**修法**:订阅事件捕获 `e.Error`,await 返回后检查并抛。
3. **磁盘不足 → OOM**:下载前 `EnsureDiskSpace` 预检(中文提示);`MaximumMemoryBufferBytes` 保持 0(无上限),健康盘上 watcher 持续落盘、内存平稳。
4. **慢盘(HDD)会停滞**:bezzad 多分片随机写 HDD(如 D 盘)跟不上 → 背压死锁。**staging 优先系统盘(SSD,≥15GB)**,否则选最大空闲盘。

staging 目录在剩余空间最大的固定盘 `VivoKsu\safe-flash\<guid>\`,下载与解包都在此,刷完清理。

## 刷写(fastboot-rs CLI)

刷写阶段用 **fastboot-rs 命令行 EXE**(`platform-tools/fastboot-rs.exe`,1.0.0),而非 DLL:

- **为什么换 EXE**:DLL 的 `fastboot_flash` C ABI 只回粗错误码(多数失败归 `-8`),拿不到原因;EXE 打印可读、可操作错误(无设备 + 检查清单 / 镜像未找到:<路径> / 设备 FAIL 消息)。
- 封装 `FastbootRsCliRunner`:子进程调 `fastboot-rs.exe -s <serial> flash <分区> <镜像>` / `getvar partition-type:<名>` / `reboot`。
- **分区存在性预检**:flash 前 `getvar partition-type:<分区>`,设备没有的分区(OTA 里区域变体专属)跳过 + 日志,避免未知分区中止整条流程导致半刷。
- **刷写模式(vivo 一律 fastbootd)**:`adb reboot fastboot`(ADB→fastbootd)→ 等待 FastbootConnected → 逐个 `fastboot flash` → `fastboot reboot`(回系统)。
- 刷写循环不再每次校验 `session.Serial == 缓存`(避免长时间刷大分区时监控瞬时抖动误判),依赖 fastboot 自身报错;用当前 `session.Serial`。

## 操作日志

右侧 ACTIVITY LOG 条目显示为 `[HH:mm:ss] 消息` 单行等宽(仿 Google-fastboot 刷机工具),按级别着色,自动滚动到底。安全刷写流程把这些行写进操作日志:

```
[12:35:46] 已选择固件 C:\...\PD2417_..._ota.zip
[12:35:46] 固件含 36 个分区(已跳过 preloader/lk)
[12:35:46] 等待Fastboot设备...
[12:36:01] 已连接 400E81010Y00000 | 用时 13 秒
[12:36:04] Sending 'boot' (8192 KB)
[12:36:04] OKAY [  0.202s]
[12:36:04] Writing 'boot'                               OKAY [  0.202s]
[12:36:04] Finished. Total time: 0.202s
[12:36:04] Flashing boot.img...OK
[12:36:04] [Rebooting]发送重启命令...
[12:36:04] 任务结束,耗时17.6秒.
```

> Sending / OKAY / Writing 的时间用每次 flash 的实测耗时(fastboot-rs EXE 是阻塞调用,拿不到分相),格式对齐即可。

## 相关文档

- `README.md` —— 项目总览 / 页面 / 服务端(Cloudflare Worker)架构。
- [vivoksu-safeflash.md](../../../.claude/projects/C--Users-17254-Desktop-TOOL-VivoKsu---/memory/vivoksu-safeflash.md) —— 会话记忆(坑与要点)。
