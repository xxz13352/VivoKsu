# 官方 KernelSU 与 Vendor Boot 设计

## 目标

移除外部 `ksud.exe` 修补链，将 ROOT 页更名为“Vivo 专用”，并提供 Vivo KSU 与官方 KernelSU 两个互斥方案。

## 流程

- Vivo KSU：复用已验证 APK 中的 `libksud.so`，只处理用户选择的 `init_boot`。
- 官方 KernelSU：必须同时提供 `init_boot` 与 `vendor_boot`。APK 中的 `libksud.so` 修补 `init_boot`；`magiskboot.so` 在设备端解包 `vendor_boot` 的 `vendor_ramdisk/ramdisk.cpio`，按用户选择的官核或 GKI 路径处理 `modules.load`、`modules.load.recovery`、`modules.softdep`，删除 `vr.ko` 和 `softdep vr pre` 对应行后重打包。
- 官方 KSU 的官核目标为 `lib/modules`；GKI 目标从 `lib/modules/<release>-gki` 检测并选择。

## 资源与门禁

- `KernelSU.apk` 和 `magiskboot.so` 从 `临时` 目录移入应用资源；运行时只检查文件存在且非空。
- 官方 KSU 不能在 `init_boot` 或 `vendor_boot` 缺失时开始。
- 所有设备端临时文件位于 `/data/local/tmp`，操作结束后删除。
- 只接受 `.img` 镜像；修补前读取当前源文件，修补后要求输出非空、大小受限。

## 移除

- 删除 `KsuPatchService`、`KsuPatchToolLocator`、`ksud.exe` 偏好项、外部修补器浏览控件及其测试。
