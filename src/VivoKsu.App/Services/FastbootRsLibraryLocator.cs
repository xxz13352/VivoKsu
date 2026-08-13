using System.IO;
using System.Runtime.InteropServices;

namespace VivoKsu.App.Services;

public sealed class FastbootRsLibraryLocator
{
    public FastbootRsLibraryLocator(string applicationRoot)
    {
        ApplicationRoot = applicationRoot;
    }

    public string ApplicationRoot { get; }

    public string LibraryPath => Path.Combine(ApplicationRoot, "platform-tools", "fastboot.dll");

    public bool IsAvailable => File.Exists(LibraryPath);

    public bool TryLoad(out IntPtr handle)
    {
        if (!IsAvailable)
        {
            handle = IntPtr.Zero;
            return false;
        }

        return NativeLibrary.TryLoad(LibraryPath, out handle);
    }
}
