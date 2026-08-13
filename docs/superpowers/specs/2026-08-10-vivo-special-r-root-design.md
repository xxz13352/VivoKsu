# Vivo 专用 R ROOT 迁移设计

## 目标

把 `mtkbl` 中与 KernelSU Root 直接相关的资源校验和设备流程迁移到现有 .NET 8 WPF 工具，并将 ROOT 导航归类为“Vivo 专用 R”。UI 布局保持不变。

## 设计

- `VivoRootResourceService` 保存 KSU/Sukisu 的固定 APK 元数据、KMI allowlist，并负责 APK SHA-256、ABI 对应 `libksud.so` 提取与哈希校验。
- `VivoRootViewModel` 继续沿用现有 `RootViewModel` 类型，新增管理器、KMI、检测 KMI、管理器 APK 状态和安装命令；现有镜像修补结果仍通过快速刷写页完成刷写和重启。
- KMI 只接受 `android13-5.15`、`android14-6.1`、`android15-6.6`。设备内核版本可映射为提示，不覆盖用户选择。
- 管理器安装使用现有 ADB backend，安装后验证包名并启动已知 Activity。所有阶段写入右侧操作日志和左下设备状态。
- `mtkbl` 的 Python GUI、解锁页面和整套 MTK flow 不复制；Vivo preload 二进制先作为受校验资源目录接入，执行链作为后续独立任务。

## 错误处理与验证

- 缺失、空文件、大小或 SHA-256 不匹配时 fail closed，不执行安装或刷写。
- 非支持 KMI、设备 ABI 不匹配、非 ADB 设备时命令保持不可执行。
- 单元测试覆盖资源解析、APK/libksud 校验、KMI 映射、ViewModel 门禁；全量 `dotnet test` 与 Release 构建作为交付验证。
