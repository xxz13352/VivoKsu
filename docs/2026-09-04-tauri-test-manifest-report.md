# Tauri Windows 测试宿主 Common Controls v6 方案与风险报告

## 任务边界

本报告记录 `e5102cc` 对 nwflash-tauri Windows 测试宿主 Common Controls v6 manifest 问题的方案、实现与验证。它不复制或替换 System32 DLL，不改变 Tauri command、Safe Flash、文件传输、Web API、发布配置或分支 refs。

代码实现前先完成了只读检查；报告随后与测试构建改动一起由总指挥审核并提交为 `e5102cc`。

## 构建入口核对

- workspace 根包是 nwflash-desktop，入口为 src/Nwflash.Desktop/src-tauri/build.rs。该脚本调用 tauri_build::build，并在 protected 构建时给 nwflash-desktop 二进制加 MAP 参数。
- nwflash-tauri 是独立 workspace member，入口为 crates/nwflash-tauri/src/lib.rs；基线时它没有自己的 build.rs 或 build-dependencies。cargo metadata 显示该包只有一个 lib test target（由 cargo test -p nwflash-tauri --lib 生成）和 mirror_runtime、release_probe 两个 integration test target。
- 因此 workspace 根包的 build.rs 不会为 nwflash-tauri 的 lib test 二进制注入 Windows resource。直接修改根 build.rs 会扩大到正式应用/发布，超出本任务边界。
- Cargo.lock 已经包含 embed-resource 3.0.11（由 tauri-winres 间接使用），因此增加 nwflash-tauri 的直接 build-dependency 不会引入未知版本或新的系统 DLL。

## 基线证据

在 src/Nwflash.Desktop/src-tauri 下执行：

| 命令 | 结果 |
|---|---|
| cargo test -p nwflash-tauri --lib --no-run | 通过；测试 EXE 成功链接并生成 |
| cargo test -p nwflash-tauri --lib -- --list | 失败；测试 EXE 启动即 0xc0000139 STATUS_ENTRYPOINT_NOT_FOUND |

失败发生在测试体运行之前，与测试逻辑无关，符合 PE 引用 comctl32 TaskDialogIndirect 但进程没有声明 Microsoft.Windows.Common-Controls 6.0 的症状。当前 Windows SDK 中存在 rc.exe 和 mt.exe，Rust/Cargo 版本为 1.98.1，满足测试目标所需的 Cargo 链接参数能力。

## 选择的实现方案

新增/修改 nwflash-tauri 的测试构建文件与依赖配置：

1. crates/nwflash-tauri/build.rs：仅在目标为 Windows 时调用 embed-resource 的 compile_for，传入空的 binary 集合以只生成 resource object；随后发出一次包级 cargo:rustc-link-arg。Cargo 的 rustc-link-arg-tests 只匹配 TargetKind::Test（integration test），不会匹配 lib unit harness，因此需要这个包级参数；本包保持 autobins=false 且没有 [[bin]]，所以当前所有可执行 target 都是测试 harness。构建脚本声明两个测试资源的 rerun-if-changed，并对必需 manifest 使用 manifest_required()，找不到资源编译器时 fail closed。
2. crates/nwflash-tauri/tests/windows-test.rc：定义 RT_MANIFEST 资源 ID 1，并引用测试 manifest XML。
3. crates/nwflash-tauri/tests/windows-test.manifest：声明 assembly dependency Microsoft.Windows.Common-Controls，version 6.0.0.0、publicKeyToken 6595b64144ccf1df、processorArchitecture=*、language=*。

Cargo.toml 只增加 nwflash-tauri 的 build-dependency embed-resource = 3.0.11（版本与现有 lock entry 对齐）。不修改 workspace 根 build.rs、src-tauri/tauri.conf.json、应用入口或任何运行时代码。

## 为什么不会污染正式发布

- 包级 `cargo:rustc-link-arg` 只由 nwflash-tauri 的 build script 产生；该包没有发布 binary，正式应用是独立 workspace 根包 nwflash-desktop 的 bin target，继续使用现有 tauri_build/build.rs 路径。必须用 verbose root build 检查参数没有传播到该 bin。
- 测试 manifest 资源位于 crates/nwflash-tauri/tests/，不在 Tauri bundle resources allowlist，也不会复制 DLL 或改写 Windows 系统组件。
- 构建后须检查测试 EXE 的 RT_MANIFEST 内容，并检查正式 nwflash-desktop 构建/链接输出没有新增测试 resource 参数或文件。

## 风险与缓解

| 风险 | 缓解/停止条件 |
|---|---|
| Cargo 版本或包级 rustc-link-arg 语义不符合预期 | 当前 rustc 1.98.1；若参数传播到正式 bin 或 unit-test 仍无资源，立即停止并退回拆分测试宿主，不扩大到 workspace 根 build.rs |
| embed-resource 找不到 rc/LLVM resource compiler | 使用 manifest_required() 使构建失败；不退回复制 DLL 或静默跳过 |
| 资源参数误作用于发布二进制 | 使用 compile_for 空集合，不调用 compile/compile_for_tests；检查 nwflash-tauri 与正式 nwflash-desktop 的 verbose link 输出 |
| 带空格/中文的工作区路径导致 /MANIFESTINPUT 参数解析错误 | 由 embed-resource 生成并传递 resource object，不手写带空格的 linker 命令；在当前工作区实际 no-run 构建验证 |
| 其他测试 target 仍无法启动 | 先区分是 manifest 缺失还是独立 DLL/运行库问题；不把一次成功扩展为发布或真机结论 |
| Windows SDK/链接器环境缺失 | 记录明确错误并停在方案报告，按计划改走无 Wry GUI 依赖的纯命令测试拆分；本机已发现 SDK rc.exe/mt.exe，暂不需要拆分 |

## 验收与证据要求

实现后按计划运行：

    cargo test -p nwflash-tauri --lib --no-run
    cargo test -p nwflash-tauri --lib -- --list
    cargo test -p nwflash-tauri --lib
    cargo test -p nwflash-tauri --tests
    cargo check --workspace --all-targets
    git diff --check

另外从生成的 nwflash-tauri 测试 EXE 提取/读取资源，确认 RT_MANIFEST 包含 Microsoft.Windows.Common-Controls 6.0；对正式 nwflash-desktop target 做非部署构建检查，确认其 manifest/链接边界未退化。任何一项无法证明“只作用于测试宿主”时停止，不继续实现。

## 当前决定

已确认 nwflash-tauri 没有发布 binary，且 verbose root build 不接收测试 resource，因此方案限定在测试构建范围内。若 Cargo.toml 以后新增发布 binary，构建脚本应停止；任何资源参数传播到正式应用 manifest 都是回归。

## 实际实现与验收记录

实现文件仅为 nwflash-tauri 的 Cargo/build/test 资源与本报告。初版同时发出 test-only 和包级参数，integration tests 因同一 resource object 重复链接触发 CVT1100/LNK1123；已改为 compile_for 空集合加单一包级参数，并重新构建。Cargo 源码的 TargetKind 过滤也解释了为何 lib unit harness 必须走包级 fallback。

| 验证 | 结果 |
|---|---|
| cargo test -p nwflash-tauri --lib --no-run | 通过 |
| cargo test -p nwflash-tauri --lib -- --list | 通过，列出 300 tests |
| cargo test -p nwflash-tauri --lib | 通过，300 passed |
| cargo test -p nwflash-tauri --tests --no-run | 通过 |
| cargo test -p nwflash-tauri --tests | 通过：lib 300 + mirror_runtime 1 + release_probe 4 |
| cargo check --workspace --all-targets | 通过 |
| git diff --check | 通过 |
| mt.exe 读取 nwflash-tauri unit/integration EXE RT_MANIFEST | 均为 Common-Controls 6.0.0.0，token 6595b64144ccf1df |
| root `nwflash-desktop` verbose debug build | 通过；根 binary 只接收自身 Tauri `resource.lib`，未接收 `windows-test.lib` |
| mt.exe 读取 debug `nwflash-desktop.exe` | 保留原有 Common-Controls 6.0.0.0 manifest；测试任务没有生成或验证正式 release EXE |

上述命令均为非部署验证；未复制 DLL，也未运行真机、安装器或正式 release 构建。生成在 target/ 和临时目录中的检查输出属于可再生产物。实现与报告已提交为 `e5102cc`，但该提交本身不构成发布验收。
