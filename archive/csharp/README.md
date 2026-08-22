# C# / WPF 历史版本归档

此目录保存冻结的 C# / WPF 奶蛙Flash桌面端。当前产品只继续更新 `src/Nwflash.Desktop/` 的 React + Tauri + Rust 客户端；归档内容不参与当前构建、发布或资源供应。

## 内容

- `VivoKsu.slnx`：原始 C# 解决方案。
- `src/VivoKsu.App/`：WPF 应用与其历史资源。
- `src/VivoKsu.Bootstrapper/`：历史 .NET 启动器。
- `tests/`：C# 单元测试与视觉采集项目。
- `scripts/`：WPF 发布、窗口自动化和历史线刷验证脚本。
- `docs/`：WPF 架构、线刷流程和迁移映射记录。

## 恢复与单独验证

从仓库根目录运行：

```powershell
dotnet build archive/csharp/VivoKsu.slnx -c Debug
dotnet test archive/csharp/tests/VivoKsu.App.Tests/VivoKsu.App.Tests.csproj -c Debug
```

也可以先进入归档根目录再运行：

```powershell
Set-Location archive/csharp
dotnet test tests/VivoKsu.App.Tests/VivoKsu.App.Tests.csproj -c Debug
```

归档项目保留了原有相对目录结构和资源副本，因此可独立检查；不要将其中的文件重新引入 Rust/Tauri 的活跃发布路径。
