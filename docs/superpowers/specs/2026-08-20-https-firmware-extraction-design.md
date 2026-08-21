# HTTP(S) 固件远程提取设计

## 目标

固件提取页允许用户粘贴任意 `http://` 或 `https://` 固件地址，并支持从远程 ZIP 中提取直接包含的 `.img`/`.bin` 文件，以及从包含 `payload.bin` 的远程固件中提取分区镜像。远程访问使用 HTTP Range 按需读取，页面不暴露服务器地址或内部临时路径。

## 现状与边界

- 本地固件提取已经通过 `firmware_inspect_local`、`firmware_extract_vivo_local` 和 payload 专用命令工作。
- 基础设施已有 `RangeHttpReader`，可读取远程 ZIP 中央目录和指定成员。
- `payload_dumper` 是受控的外部工具，PAYLOAD 参数可直接接受本地路径或 HTTP(S) URL，并在远程来源上按 Range 读取所选分区。
- 只允许输入 URL 的 scheme 为 `http` 或 `https`；空地址、非法 URL 和其他 scheme 均拒绝。
- 用户可以粘贴带查询参数和片段前的签名 URL；返回给前端的 DTO 不包含完整 URL。

## 方案

### 前端

固件提取页增加来源切换：本地文件和 HTTP(S) 地址。远程模式提供 URL 输入、检查和提取按钮；本地模式保持现有行为。检查成功后复用现有分区列表和不透明 entry ID，用户选择后复用输出目录选择。

远程命令返回与本地检查相同结构的 `format`、`entries`，提取命令返回与本地提取相同结构的镜像元数据。页面只显示格式、分区名、大小、进度和结果数量，不显示 URL、下载缓存路径或服务器响应文本。

### Rust 命令

新增 `firmware_inspect_remote` 和 `firmware_extract_remote` Tauri 命令，并在 `lib.rs` 注册。命令层负责参数校验、资源准备、操作取消和 opaque ID 状态；远程格式探测与 ZIP 成员提取放在基础设施层复用已有实现。

远程检查按以下顺序处理：

1. 校验 HTTP(S) URL。
2. 使用远程格式探测识别裸 payload、payload ZIP、直接镜像 ZIP 或不支持格式。
3. 直接镜像 ZIP 使用 Range 读取成员列表，只返回 `.img`/`.bin` 的安全成员名和大小。
4. payload ZIP 或裸 payload 将原始 URL 传给既有 `payload_dumper` 元数据读取流程，由工具按 Range 读取必要的元数据和分区块。
5. 检查失败时返回稳定的用户可读错误，不把完整 URL、工具临时输出或内部路径回传到页面。

远程提取遵循检查结果中的 entry ID，只允许提取已检查且仍属于当前操作的成员。直接镜像 ZIP 按 Range 读取选中成员；payload 类型将原始 URL 交给既有 `payload_dumper`，使用分区过滤参数只提取选中分区，不先下载完整远程包。取消或失败由操作协调器终止工具调用并清理工具输出。

### Range 回退

直接镜像 ZIP 必须支持 Range；若服务器不返回有效的部分内容响应，命令返回明确的“服务器不支持分块读取”错误，不隐式下载整个未知大文件。payload 类型由 `payload_dumper` 使用其远程 Range 实现读取所选分区，不经 Rust 下载器暂存完整包。

### 安全与日志

- URL 只在 Rust 内部使用，不写入操作日志，不通过错误消息回传完整 URL。
- Rust 的远程 ZIP 请求复用默认 HTTP(S) 客户端和超时设置；用户不能自定义代理、证书校验或外部工具执行参数。
- ZIP 成员名和分区名继续经过现有安全校验，拒绝目录穿越和危险外部工具参数。
- 日志只保留“检查远程固件”“提取远程固件”这类操作阶段，不打印重复服务器探测请求。

## 测试策略

- 基础设施测试覆盖 HTTP(S) URL 校验、直接镜像 ZIP 的 Range 列表和成员提取。
- Tauri 命令测试覆盖远程检查和提取只接受已检查 ID，以及错误不包含完整 URL。
- 应用层测试覆盖 payload URL 直传给外部工具且仅请求所选分区；React 测试覆盖来源切换、URL 检查、选择镜像、输出目录和命令参数；同时验证 URL 不出现在页面文本中。
- 完成后运行相关 Vitest、Rust workspace 测试、`cargo fmt --check`、TypeScript 构建和 Tauri 编译。

## 不采用的方案

- 不把所有远程 ZIP 无条件完整下载；直接镜像 ZIP 按需 Range 提取可显著减少流量。
- 不自行实现完整 payload 解析器；现有 `payload_dumper` 兼容性更成熟，并可直接处理远程 URL、按需 Range 读取。
- 不支持 `ftp://`、本地文件 URL 和其他非 HTTP(S) 协议，避免把不受控来源交给远程固件流程。
