using System.IO;
using System.Runtime.InteropServices;

namespace VivoKsu.App.Services;

public sealed class FastbootRsApiFactory
{
    private readonly FastbootRsLibraryLocator libraryLocator;
    private readonly Func<IFastbootRsNativeApi> nativeFactory;
    private readonly Func<IFastbootRsNativeApi> platformToolsFactory;

    public FastbootRsApiFactory(
        FastbootRsLibraryLocator libraryLocator,
        Func<IFastbootRsNativeApi> nativeFactory,
        Func<IFastbootRsNativeApi> platformToolsFactory)
    {
        this.libraryLocator = libraryLocator;
        this.nativeFactory = nativeFactory;
        this.platformToolsFactory = platformToolsFactory;
    }

    public IFastbootRsNativeApi Create()
    {
        var platformTools = platformToolsFactory();
        // A present-but-unloadable fastboot.dll must not permanently break the
        // fastboot backend, so probe actual loadability before choosing native.
        if (libraryLocator.TryLoad(out var handle))
        {
            if (handle != IntPtr.Zero)
            {
                NativeLibrary.Free(handle);
            }

            return new FastbootRsApiWithPlatformDeviceDiscovery(nativeFactory(), platformTools);
        }

        return platformTools;
    }

    public static IFastbootRsNativeApi CreateDefault()
    {
        var applicationRoot = AppContext.BaseDirectory;
        var executableLocator = new PlatformToolsExecutableLocator(applicationRoot);
        var adb = executableLocator.Resolve("adb.exe");
        var fastboot = executableLocator.Resolve("fastboot.exe");
        return new FastbootRsApiFactory(
            new FastbootRsLibraryLocator(applicationRoot),
            static () => new NativeFastbootRsApi(),
            () => new PlatformToolsNativeApi(new SystemPlatformToolsCommandRunner(), adb, fastboot)).Create();
    }

}

internal sealed class FastbootRsApiWithPlatformDeviceDiscovery : IFastbootRsNativeApi
{
    private readonly IFastbootRsNativeApi nativeApi;
    private readonly IFastbootRsNativeApi platformToolsApi;

    public FastbootRsApiWithPlatformDeviceDiscovery(IFastbootRsNativeApi nativeApi, IFastbootRsNativeApi platformToolsApi)
    {
        this.nativeApi = nativeApi;
        this.platformToolsApi = platformToolsApi;
    }

    // ADB's server is authoritative for debugging sessions; native fastboot-rs remains the fastboot backend.
    public string ListDevices() => platformToolsApi.ListDevices();

    public string Shell(string? serial, string command) => platformToolsApi.Shell(serial, command);

    public string GetVar(string? serial, string variable) => nativeApi.GetVar(serial, variable);

    public void Reboot(string? serial, string target) => platformToolsApi.Reboot(serial, target);

    public void FastbootReboot(string? serial) => nativeApi.FastbootReboot(serial);

    public void SetActive(string? serial, string slot) => nativeApi.SetActive(serial, slot);

    public void Push(string? serial, string localPath, string remotePath) => platformToolsApi.Push(serial, localPath, remotePath);

    public long Pull(string? serial, string remotePath, string localPath) => platformToolsApi.Pull(serial, remotePath, localPath);

    public string Install(string? serial, string apkPath, bool replace) => platformToolsApi.Install(serial, apkPath, replace);

    public void Flash(string? serial, string partition, string imagePath) => nativeApi.Flash(serial, partition, imagePath);

    public void Erase(string? serial, string partition) => nativeApi.Erase(serial, partition);

    public long Fetch(string? serial, string partition, string outputPath) => nativeApi.Fetch(serial, partition, outputPath);
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
