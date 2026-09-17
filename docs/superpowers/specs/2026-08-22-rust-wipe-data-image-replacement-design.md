# Rust 线刷清除数据镜像替换设计

## 目标

让 Rust/Tauri 版 VIVO 线刷的清除数据流程使用逆向得到的 `misc_bcb_native_wipe_data_all.img`，并保持现有代码通过内置资源写入 `misc` 分区的流程不变。

## 范围

- 替换 `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/assets/wipe-data.img` 的二进制内容。
- 保留 `embedded_assets.rs` 的 `include_bytes!` 引用、资源文件名和 Rust 线刷执行逻辑。
- 不修改 `src/VivoKsu.App/Assets/wipe-data.img`，不修改 WPF 实现及任何非 Rust 文件。

## 数据流

构建时由 `embedded_assets.rs` 将目标镜像编译进 Rust 基础设施 crate；执行清除数据时，现有 `write_wipe_data_image` 将内置字节写入临时 `wipe-data.img`，再由现有流程刷入 `misc`。本次仅改变内置字节来源，不改变路径、分区或时序。

## 验证

1. Rust 资源文件存在，大小与源文件一致。
2. Rust 资源文件 SHA-256 与源文件一致，且与替换前不同。
3. `embedded_assets.rs` 仍引用同一资源路径，工作区不产生 WPF 资源变更。
4. 运行 Rust 基础设施/应用相关测试或至少执行可用的 Rust 编译检查。
