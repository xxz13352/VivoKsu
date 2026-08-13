using System;
using System.Runtime.InteropServices;

namespace VivoKsu.App.Services;

public sealed class NativeFastbootRsApi : IFastbootRsNativeApi
{
    private const int BufferSize = 64 * 1024;
    private static readonly object InitializationLock = new();
    private static bool initialized;

    public string ListDevices()
    {
        EnsureInitialized();
        return ReadBuffer(FastbootRsNative.fastboot_devices);
    }

    public string Shell(string? serial, string command)
    {
        EnsureInitialized();
        return ReadBuffer((buffer, length) => FastbootRsNative.adb_shell(serial, command, buffer, length));
    }

    public string GetVar(string? serial, string variable)
    {
        EnsureInitialized();
        return ReadBuffer((buffer, length) => FastbootRsNative.fastboot_getvar(serial, variable, buffer, length));
    }

    public void Reboot(string? serial, string target)
    {
        EnsureInitialized();
        ThrowForNativeError(FastbootRsNative.adb_reboot(serial, target), "重启设备");
    }

    public void FastbootReboot(string? serial)
    {
        EnsureInitialized();
        ThrowForNativeError(FastbootRsNative.fastboot_reboot(serial, null), "Fastboot 重启设备");
    }

    public void SetActive(string? serial, string slot)
    {
        EnsureInitialized();
        ThrowForNativeError(FastbootRsNative.fastboot_set_active(serial, slot), "切换活动槽位");
    }

    public void Push(string? serial, string localPath, string remotePath)
    {
        EnsureInitialized();
        ThrowForNativeError(FastbootRsNative.adb_push(serial, localPath, remotePath), "上传文件");
    }

    public long Pull(string? serial, string remotePath, string localPath)
    {
        EnsureInitialized();
        var result = FastbootRsNative.adb_pull(serial, remotePath, localPath);
        if (result < 0)
        {
            throw new FastbootRsNativeException("下载文件", (int)result);
        }

        return result;
    }

    public string Install(string? serial, string apkPath, bool replace)
    {
        EnsureInitialized();
        return ReadBuffer((buffer, length) => FastbootRsNative.adb_install(serial, apkPath, replace ? 1 : 0, buffer, length));
    }

    public void Flash(string? serial, string partition, string imagePath)
    {
        EnsureInitialized();
        ThrowForNativeError(FastbootRsNative.fastboot_flash(serial, partition, imagePath, IntPtr.Zero), "刷写分区");
    }

    public void Erase(string? serial, string partition)
    {
        EnsureInitialized();
        ThrowForNativeError(FastbootRsNative.fastboot_erase(serial, partition), "擦除分区");
    }

    public long Fetch(string? serial, string partition, string outputPath)
    {
        EnsureInitialized();
        var result = FastbootRsNative.fastboot_fetch(serial, partition, outputPath);
        if (result < 0)
        {
            throw new FastbootRsNativeException("读取分区", (int)result);
        }

        return result;
    }

    private static void EnsureInitialized()
    {
        lock (InitializationLock)
        {
            if (initialized)
            {
                return;
            }

            try
            {
                var result = FastbootRsNative.fastboot_init(FastbootRsNative.fastboot_get_token());
                if (result is not 0 and not -2)
                {
                    throw new FastbootRsNativeException("初始化 fastboot-rs", result);
                }

                initialized = true;
            }
            catch (DllNotFoundException exception)
            {
                throw new InvalidOperationException("未找到 platform-tools\\fastboot.dll。请先构建 fastboot-rs 并复制 DLL。", exception);
            }
        }
    }

    private static string ReadBuffer(Func<IntPtr, nuint, int> call)
    {
        var buffer = Marshal.AllocHGlobal(BufferSize);
        try
        {
            var result = call(buffer, BufferSize);
            if (result < 0)
            {
                throw new FastbootRsNativeException("读取 native 输出", result);
            }

            return Marshal.PtrToStringUTF8(buffer, result) ?? string.Empty;
        }
        finally
        {
            Marshal.FreeHGlobal(buffer);
        }
    }

    private static void ThrowForNativeError(int result, string operation)
    {
        if (result < 0)
        {
            throw new FastbootRsNativeException(operation, result);
        }
    }
}

public sealed class FastbootRsNativeException : Exception
{
    public FastbootRsNativeException(string operation, int errorCode)
        : base($"{operation}失败，fastboot-rs 返回错误码 {errorCode}。")
    {
        ErrorCode = errorCode;
    }

    public int ErrorCode { get; }
}
