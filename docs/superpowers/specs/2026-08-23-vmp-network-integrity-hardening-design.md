# VMP、本地完整性与网络验证加固设计

## 目标

在桌面功能继续完全本地执行的前提下，提高对客户端 dump、静态分析、动态补丁、代理抓包和登录/心跳绕过的抵抗能力。服务器只负责认证、签发短期能力租约、刷新心跳租约和接收完整性遥测，不执行设备功能。

本设计不宣称能在攻击者完全控制的 Windows 主机上绝对阻止 dump。安全目标是：客户端内不保存可伪造服务器授权的秘密；dump 后仍无法生成有效服务器租约；本地关键判断通过多层独立校验和 VMP 虚拟化提高补丁成本。

## 总体架构

保护分为六层：

1. Cloudflare 使用 Ed25519 私钥签发登录、心跳和 pin 清单。
2. Rust 客户端使用编译进二进制的公钥验证签名。
3. NWflash API 使用独立 Rustls 客户端，在标准 WebPKI/域名校验后叠加 SPKI pin。
4. 登录接纳、心跳分类、本地操作准入和篡改失败处理进入同步黑盒策略分发器。
5. VMP 只虚拟化这些同步叶子函数，并提供 `VMProtectIsValidImageCRC`。
6. protected release 使用更严格的 Cargo profile、VMP 后签名、精确资源清单和安装器验收。

React/WebView 只负责凭据输入和状态展示。token、租约、公钥验证、pin、CRC、退出编排和所有设备命令均保留在 Rust。

## 服务器签名租约

### 签名格式

服务器返回：

- `lease_payload`：base64url 编码的 UTF-8 JSON。
- `lease_signature`：对原始 `lease_payload` ASCII 字节进行 Ed25519 签名后得到的 base64url 字符串。

客户端先验签，再解析 payload。这样不依赖跨语言 JSON 字段顺序。

租约 claims 固定包含：

- `version = 1`
- `kind`：`login` 或 `heartbeat`
- `username`
- `token_sha256`
- `client_version`
- `build_id`
- `process_nonce`
- `session_id`
- `sequence`
- `issued_at`
- `expires_at`

登录请求新增 `process_nonce` 和 `build_id`。心跳请求新增当前 sequence。服务器按当前 epoch 生成递增 sequence 和短期租约。客户端拒绝签名无效、字段不匹配、租约过期、sequence 回退、错误 build 或错误 process nonce。

生产 Ed25519 私钥只通过 Cloudflare Secret 提供；仓库只保存公钥和测试夹具。未配置生产密钥时 Worker 和正式客户端构建必须失败关闭，不能回退到无签名租约。

## TLS 与 CA/SPKI 固定

仅 `api.nwflash.cc.cd` 使用 pinned client。固件、ROM 和其他第三方下载继续使用普通独立客户端。自定义 root、pin、resolver、测试验签公钥和无 pin HTTP adapter 只在 debug/test 编译中存在，release 对外只保留精确生产 pinned 构造路径。

握手顺序：

1. Rustls/WebPKI 校验证书链、DNS 名、有效期和握手签名。
2. 要求 SNI/主机名严格等于 `api.nwflash.cc.cd`。
3. 解析叶证书与服务端发送的中间证书。
4. 至少一个 SPKI SHA-256 匹配经过签名批准的 pin 集合。

初始内置 pin：

- 叶 SPKI：`kavrs5Bk3Tjn+0G+uPjWGBqJsXzW5kHFNPzgxuvrcKY=`，当前证书到期 2026-11-11。
- Google Trust Services WE1 中间 SPKI：`kIdp6NNEd8wsugYyyIYFsi1ylMCED3hZbSR8ZFsa/A4=`，当前证书到期 2029-02-20。

客户端保留 WebPKI 验证，不使用 `danger_accept_invalid_certs`，不信任 Cloudflare Origin CA，不允许 Windows 本地新增 CA 单独绕过 pin。Pinned client 禁用代理自动发现、自动重定向和 TLS key log；任何 3xx、跨 host 或降级到 HTTP 的响应都映射为 endpoint-policy 完整性失败。

### Pin 轮换

新增公共 `/api/security/pins`，返回 Ed25519 签名的 pin 清单，字段包括 `version`、`not_before`、`expires_at`、主 pin 和备用 pin。客户端只接受：

- 签名有效。
- version 不低于 release 内置 floor，且不低于本进程已验签/安装的最高版本；同版本不得替换为不同 payload。
- `not_before` / `expires_at` 有效，并在加载、安装和每次 API 操作前重新检查；不能因连接池复用跳过时间检查。
- host 精确匹配。
- 至少保留一个当前可信 pin，或由仍有效的内置 Ed25519 公钥签发。

签名 pin 清单可以缓存到本地；缓存不是秘密，加载时必须重新验签。客户端只持久化签名 envelope，不持久化私密状态或伪装成安全计数器。攻击者控制本机时，可以在进程启动前把缓存替换成另一个签名有效、时间有效且不低于内置 floor 的旧 envelope；仅靠该公共文件无法识别这种替换。因此“单调”明确只覆盖本进程最高已验证版本和 release 内置 floor，不宣称攻击者控制主机时存在 tamper-proof 跨启动单调存储。Task 8 发布时按需要同步提升内置 floor 和验签公钥。

动态 pinset 过期或尚未生效时，普通 API 操作返回 `PinsetTime`。刷新可以使用仅含 WebPKI + 内置 bootstrap pin 的独立 pinned client 恢复新签名 pinset；该路径仍禁用代理、重定向和 key log，且不能绕过内置 pin。缓存更新使用同目录临时文件、flush/sync 和原子替换，替换失败保留旧 envelope。

pin 失配映射为完整性事件，而不是普通网络错误。由于 pin 失配时当前 API 通道已经不可信，客户端不能保证把该事件送达同一主机；只有独立遥测 host 的 pin 仍有效时才尝试上报，否则直接退出并保留本地最小事件标记。

## Rust 黑盒保护模块

新增独立、无 async、无 HTTP、无 Tauri 类型的 protection 模块。它消费规范化输入，输出封闭决策枚举。

核心函数：

- `verify_signed_lease`
- `accept_login_lease`
- `classify_heartbeat_lease`
- `admit_local_operation`
- `verify_image_integrity`
- `dispatch_protection_decision`

关键函数使用稳定导出名、`inline(never)` 和封闭整数 selector，便于 VMP 精确选择。dispatcher 不接收明文密码或 bearer token，只接收摘要、时间、序号、reason code 和验证后的 flags。

同一能力在两个独立边界复核：登录成功时创建 session capability；每次敏感 command 开始前重新检查租约。修改 React 状态或跳过一个分支不能直接获得设备能力。

## VMP SDK 与保护范围

VMP SDK 不提交到仓库。protected build 通过外部 `NWFLASH_VMP_SDK_ROOT` 定位 x64 Include/Lib，并用 Cargo feature 启用 FFI；普通开发和测试构建使用可注入的无 VMP probe。

VMP 标记：

- 登录租约验签与接纳：Ultra。
- 心跳租约分类：Virtualization。
- 操作准入与黑盒分发：Ultra。
- CRC 与篡改失败分发：Virtualization。
- build identity 检查：Mutation。

启用 Memory Protection、Import Protection 和 Packing。禁止对下列内容做整体虚拟化：

- Tauri/WebView 入口。
- Tokio runtime 或 async 状态机。
- HTTP/TLS 握手。
- adb、fastboot、驱动和子进程控制。
- 下载、固件解包和设备写入循环。
- 第三方依赖。

不启用虚拟机禁止执行。反调试检测只可在登录/空闲安全点作为遥测，不在设备写入期间触发强退。

## 完整性检查与退出策略

本地 CRC、Authenticode、发布清单或登录租约签名命中篡改时：

1. 构造不含 token、密码、路径、URL、设备 serial 的最小事件。
2. 已登录时携带 token 上报；未登录时调用匿名完整性端点。SPKI 失配仅在独立遥测 pin 仍有效时尝试上报。
3. 上报总超时不超过 750 ms，不重试。
4. 无论上报是否成功，调用 `std::process::exit` 直接结束本地进程。

CRC 只在启动、登录接纳、会话恢复和高风险操作开始前调用，不在刷写/分区写入中轮询。

心跳处理：

- 401、403、426、force-exit、心跳租约签名无效、sequence 回退：立即进入 `ExitPending`。
- 普通超时或传输错误连续三次进入 `ExitPending`。
- `ExitPending` 立即拒绝新操作。
- 无任务时立即完成 goodbye、上报和退出。
- 有任务时不取消当前任务；等待 OperationCoordinator 回到 idle 后完成 goodbye、清理 token/capability 并由 Rust 主进程退出。

退出决策不依赖 React event handler。

## 完整性遥测端点

新增 `/api/integrity/report`：

- 允许匿名或 bearer token 绑定用户。
- 只接受固定 phase/reason 枚举、event ID、client version、build ID 和时间。
- 请求体设置严格大小上限。
- 按 IP 和时间窗口限流，D1 幂等去重。
- 匿名事件标记为 untrusted telemetry，不能直接触发封号。
- 后台只展示脱敏字段，不展示密码、token、设备 serial、文件路径或原始外部输出。

## 凭据与内存

- token 使用 Rust secret/zeroize 包装，替换、登出和退出时清理。
- React 在发起登录后立即清空密码状态；token 永不返回 WebView。
- 日志、panic、错误边界和遥测不得包含凭据。
- 不把 HMAC 私钥、VMP 许可证、代码签名证书或 Ed25519 私钥放进客户端或仓库。

## Tauri 与前端边界

- 正式 capability 移除 WDIO/WebDriver 权限；E2E 使用单独构建配置。
- 保持严格 CSP，不加载远程脚本。
- 自定义 command 必须在 Rust 内检查 protection capability。
- 正式构建校验 E2E bridge/plugin 不进入产物。
- React 的 `has_token` 仅是显示信息，不是授权依据。

## Protected release profile

正式 protected build 使用：

- fat LTO。
- `codegen-units = 1`。
- `panic = abort`。
- 禁用增量编译。
- VMP 前保留定位标记所需的 PDB/符号，不把它们放入 release。
- VMP 处理后再签名 EXE。
- 以签名 EXE 重新构建并签名 NSIS。
- 最终清单拒绝未保护 EXE、PDB、MAP、SDK DLL、额外文件或无效签名。

Lite GUI 不提供官方控制台自动化。仓库同时支持手工保护交接：准备未签名 EXE/PDB、记录输入哈希、人工 VMP 输出到新文件、验证输出非空且哈希变化，再进入签名和 NSIS 阶段。若受控环境提供真正 console，则继续使用自动保护脚本。

## 验证

必须覆盖：

- Ed25519 有效/错误签名、字段篡改、过期、nonce/build 不匹配和 sequence 回退。
- TLS 正常链+正确 pin、正常链+错误 pin、本地代理 CA、错误域名、过期证书和 pin 轮换。
- 未登录/已登录篡改上报、750 ms 超时和无条件退出分发。
- 心跳显式失败、三次传输失败、任务空闲立即退出和任务完成后退出。
- 登录/心跳 dispatcher 的所有 selector 与非法 selector。
- protected feature 的 VMP probe 注入与普通构建 no-op probe。
- Rust workspace、React、Cloudflare、Tauri native E2E、生产前端和 release contract。
- 手工 VMP 版启动、登录、心跳、CRC、操作准入、adb/fastboot/下载和 NSIS 安装卸载。

## 非目标与限制

- 不使用内核驱动或 Protected Process Light 阻止 dump。
- 不开启会破坏 WebView2/VMP 的全局动态代码禁止策略。
- 不把 API 地址字符串混淆当作安全边界。
- 不承诺阻止管理员、内核级或硬件级攻击者取得进程内存。
- 不因保护失败在正在进行的设备写入中途主动取消或杀子进程。
