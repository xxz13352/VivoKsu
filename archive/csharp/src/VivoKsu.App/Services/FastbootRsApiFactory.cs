using System.IO;

namespace VivoKsu.App.Services;

/// <summary>
/// 构造 fastboot-rs 后端使用的 <see cref="IFastbootRsNativeApi"/>。
/// 已移除 fastboot-rs 原生 DLL(fastboot.dll)依赖:全部 fastboot 刷写 / 读变量 /
/// 擦除 / 重启 / 槽位操作改由 <see cref="FastbootCliRunner"/> 走唯一 fastboot.exe 完成,
/// 此处只提供 ADB(设备发现 / shell / 文件传输 / 安装)基础能力。
/// </summary>
public sealed class FastbootRsApiFactory
{
    public static IFastbootRsNativeApi CreateDefault()
    {
        var applicationRoot = AppContext.BaseDirectory;
        var executableLocator = new PlatformToolsExecutableLocator(applicationRoot);
        var adb = executableLocator.Resolve("adb.exe");
        var fastboot = executableLocator.Resolve("fastboot.exe");
        return new PlatformToolsNativeApi(new SystemPlatformToolsCommandRunner(), adb, fastboot);
    }
}

public sealed class PlatformToolsExecutableLocator
{
    public PlatformToolsExecutableLocator(string applicationRoot)
    {
        ApplicationRoot = applicationRoot;
    }

    public string ApplicationRoot { get; }

    public string Resolve(string executable)
    {
        var vendoredPath = Path.Combine(ApplicationRoot, "platform-tools", executable);
        return File.Exists(vendoredPath) ? vendoredPath : executable;
    }
}
