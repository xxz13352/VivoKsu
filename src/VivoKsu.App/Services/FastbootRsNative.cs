using System.Runtime.InteropServices;
using System.Reflection;

namespace VivoKsu.App.Services;

internal static class FastbootRsNative
{
    private const string LibraryName = "fastboot.dll";

    static FastbootRsNative()
    {
        NativeLibrary.SetDllImportResolver(typeof(FastbootRsNative).Assembly, ResolveLibrary);
    }

    private static IntPtr ResolveLibrary(string libraryName, Assembly assembly, DllImportSearchPath? searchPath)
    {
        if (!string.Equals(libraryName, LibraryName, StringComparison.OrdinalIgnoreCase))
        {
            return IntPtr.Zero;
        }

        var locator = new FastbootRsLibraryLocator(AppContext.BaseDirectory);
        return locator.TryLoad(out var handle) ? handle : IntPtr.Zero;
    }

    [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern ulong fastboot_get_token();

    [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int fastboot_init(ulong token);

    [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern void fastboot_cleanup();

    [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int fastboot_devices(IntPtr outputBuffer, nuint bufferLength);

    [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl, CharSet = CharSet.Ansi)]
    internal static extern int adb_shell(
        [MarshalAs(UnmanagedType.LPUTF8Str)] string? serial,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string command,
        IntPtr outputBuffer,
        nuint bufferLength);

    [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl, CharSet = CharSet.Ansi)]
    internal static extern int fastboot_getvar(
        [MarshalAs(UnmanagedType.LPUTF8Str)] string? serial,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string variable,
        IntPtr outputBuffer,
        nuint bufferLength);

    [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl, CharSet = CharSet.Ansi)]
    internal static extern int adb_reboot(
        [MarshalAs(UnmanagedType.LPUTF8Str)] string? serial,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string? target);

    [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl, CharSet = CharSet.Ansi)]
    internal static extern int fastboot_reboot(
        [MarshalAs(UnmanagedType.LPUTF8Str)] string? serial,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string? target);

    [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl, CharSet = CharSet.Ansi)]
    internal static extern int fastboot_set_active(
        [MarshalAs(UnmanagedType.LPUTF8Str)] string? serial,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string slot);

    [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl, CharSet = CharSet.Ansi)]
    internal static extern int adb_push(
        [MarshalAs(UnmanagedType.LPUTF8Str)] string? serial,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string localPath,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string remotePath);

    [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl, CharSet = CharSet.Ansi)]
    internal static extern long adb_pull(
        [MarshalAs(UnmanagedType.LPUTF8Str)] string? serial,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string remotePath,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string localPath);

    [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl, CharSet = CharSet.Ansi)]
    internal static extern int adb_install(
        [MarshalAs(UnmanagedType.LPUTF8Str)] string? serial,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string apkPath,
        int replace,
        IntPtr outputBuffer,
        nuint bufferLength);

    [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl, CharSet = CharSet.Ansi)]
    internal static extern int fastboot_flash(
        [MarshalAs(UnmanagedType.LPUTF8Str)] string? serial,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string partition,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string imagePath,
        IntPtr callback);

    [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl, CharSet = CharSet.Ansi)]
    internal static extern int fastboot_erase(
        [MarshalAs(UnmanagedType.LPUTF8Str)] string? serial,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string partition);

    [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl, CharSet = CharSet.Ansi)]
    internal static extern long fastboot_fetch(
        [MarshalAs(UnmanagedType.LPUTF8Str)] string? serial,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string partition,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string outputPath);
}
