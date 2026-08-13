using VivoKsu.App.Services;

namespace VivoKsu.App.Tests;

public class FastbootRsLibraryLocatorTests
{
    [Fact]
    public void Resolves_the_vendored_library_from_the_application_root()
    {
        var locator = new FastbootRsLibraryLocator("C:\\VivoKsu");

        Assert.Equal("C:\\VivoKsu\\platform-tools\\fastboot.dll", locator.LibraryPath);
    }

    [Fact]
    public void Does_not_load_when_the_vendored_library_is_missing()
    {
        var temporaryRoot = Path.Combine(Path.GetTempPath(), Guid.NewGuid().ToString("N"));
        var locator = new FastbootRsLibraryLocator(temporaryRoot);

        var loaded = locator.TryLoad(out var handle);

        Assert.False(loaded);
        Assert.Equal(IntPtr.Zero, handle);
    }

    [Fact]
    public void Loads_the_packaged_fastboot_rs_native_library()
    {
        var locator = new FastbootRsLibraryLocator(AppContext.BaseDirectory);

        var loaded = locator.TryLoad(out var handle);

        Assert.True(loaded);
        Assert.NotEqual(IntPtr.Zero, handle);
        Assert.True(System.Runtime.InteropServices.NativeLibrary.TryGetExport(handle, "fastboot_init", out _));
        Assert.True(System.Runtime.InteropServices.NativeLibrary.TryGetExport(handle, "fastboot_flash", out _));
        Assert.True(System.Runtime.InteropServices.NativeLibrary.TryGetExport(handle, "adb_install", out _));
        System.Runtime.InteropServices.NativeLibrary.Free(handle);
    }
}
