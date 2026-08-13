using VivoKsu.App.Services;

namespace VivoKsu.App.Tests;

public sealed class VivoRootResourceServiceTests
{
    [Fact]
    public void Exposes_the_mtkbl_kernel_su_kmi_allowlist()
    {
        Assert.Equal(
            ["android13-5.15", "android14-6.1", "android15-6.6"],
            VivoRootResourceService.SupportedKmis);
    }

    [Theory]
    [InlineData("5.15.148", "android13-5.15")]
    [InlineData("6.1.75-android14", "android14-6.1")]
    [InlineData("6.6.12", "android15-6.6")]
    public void Maps_supported_kernel_releases_to_the_same_kmi_family(string release, string expected)
    {
        Assert.Equal(expected, VivoRootResourceService.MapKernelRelease(release));
    }

    [Fact]
    public void Rejects_an_unlisted_kmi()
    {
        Assert.Throws<ArgumentException>(() => VivoRootResourceService.ValidateKmi("android16-7.0"));
    }

    [Fact]
    public void Resolves_the_two_verified_manager_catalog_entries()
    {
        var root = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(Path.Combine(root, "apk"));

        try
        {
            var service = new VivoRootResourceService(root);
            var ksu = service.ResolveManager("KSU");
            var official = service.ResolveManager("OfficialKsu");

            Assert.Equal("me.inkdye.vivoksu", ksu.PackageName);
            Assert.Equal("me.weishu.kernelsu", official.PackageName);
            Assert.Equal("KSU.APK", Path.GetFileName(ksu.ApkPath));
            Assert.Equal("KernelSU.apk", Path.GetFileName(official.ApkPath));
        }
        finally
        {
            Directory.Delete(root, true);
        }
    }

    [Fact]
    public void Verifies_and_extracts_libksud_from_the_bundled_kSU_apk()
    {
        var apk = Path.Combine(AppContext.BaseDirectory, "apk", "KSU.APK");
        Assert.True(File.Exists(apk), $"Missing test APK: {apk}");
        var root = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(root);

        try
        {
            var service = new VivoRootResourceService(AppContext.BaseDirectory);
            var manager = service.ResolveManager("KSU");
            var extracted = service.ExtractVerifiedLibKsud(manager, "arm64-v8a", Path.Combine(root, "libksud.so"));

            Assert.True(File.Exists(extracted));
            Assert.True(new FileInfo(extracted).Length > 0);
        }
        finally
        {
            Directory.Delete(root, true);
        }
    }
}
