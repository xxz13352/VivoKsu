# 内置 Tauri 运行时资源实施计划

**目标：** 将当前 Tauri 客户端实际使用的固定工具资源随包发布，并移除生产路径中的按需资源下载。

## 范围

- 内置：完整 `scrcpy` 运行目录、`KSU.APK`、`KernelSU.apk`、`payload_dumper.exe`，以及已有 platform-tools、驱动、magiskboot。
- 保留网络：ROM/OTA 内容读取、服务端认证/版本更新；它们不是可随包的静态工具资源。
- 不内置未被当前 Tauri manager key 使用的旧 `Sukisu.APK`。
- 保留发行/受控资源完整性校验；不恢复运行时镜像哈希或跨步骤设备 serial 绑定。

## 实施顺序

1. 用现有 WPF 受控资源生成 Tauri `resources/` 文件和 scrcpy 完整性清单，并把所有文件加入 Tauri 与发布 allowlist。
2. 为 payload_dumper 与 scrcpy 增加只使用给定 bundle 根目录的 provisioner 构造方式；生产 command 传入 `bundled_resource_root()`，不配置远程下载器。
3. ROOT manager 资源改为 bundle-only；资源盘点/检查命令改为校验内置组件，移除下载阶段文案。
4. 更新前端组件检查页面、投屏提示、发布验证脚本和说明文档；删除宣传性注释/文案，保留账号授权实现。
5. 运行 Rust/React 回归、release 打包资源验证和最终 Tauri release EXE 构建。
