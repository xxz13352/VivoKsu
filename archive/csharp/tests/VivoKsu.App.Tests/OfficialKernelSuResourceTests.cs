using VivoKsu.App.Services;

namespace VivoKsu.App.Tests;

public sealed class OfficialKernelSuResourceTests
{
    [Fact]
    public void Resolves_the_official_KernelSU_apk_with_its_verified_identity()
    {
        var resources = new VivoRootResourceService(AppContext.BaseDirectory);
        var manager = resources.ResolveManager("OfficialKsu");

        Assert.Equal("me.weishu.kernelsu", manager.PackageName);
        Assert.Equal("me.weishu.kernelsu.ui.MainActivity", manager.ActivityName);
        Assert.Equal("KernelSU.apk", Path.GetFileName(manager.ApkPath));
        resources.VerifyManagerApk(manager);
    }

    [Fact]
    public void Rejects_a_tampered_manager_apk()
    {
        var root = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        var apkDirectory = Path.Combine(root, "apk");
        Directory.CreateDirectory(apkDirectory);
        var original = Path.Combine(AppContext.BaseDirectory, "apk", "KernelSU.apk");
        var replacement = Path.Combine(apkDirectory, "KernelSU.apk");
        File.Copy(original, replacement);
        File.AppendAllText(replacement, "replacement");

        try
        {
            var resources = new VivoRootResourceService(root);

            // 篡改过的 APK(SHA-256 不匹配)必须被拒绝,绝不能带 root 安装。
            Assert.Throws<InvalidDataException>(
                () => resources.VerifyManagerApk(resources.ResolveManager("OfficialKsu")));
        }
        finally
        {
            Directory.Delete(root, true);
        }
    }

    [Fact]
    public void Resolves_the_verified_magiskboot_binary_for_vendor_boot_processing()
    {
        var resources = new VivoRootResourceService(AppContext.BaseDirectory);
        var tool = resources.ResolveMagiskboot();

        resources.VerifyRootTool(tool);
    }

    [Fact]
    public void Extracts_the_official_KernelSU_arm64_libksud()
    {
        var root = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(root);

        try
        {
            var resources = new VivoRootResourceService(AppContext.BaseDirectory);
            var library = resources.ExtractVerifiedLibKsud(
                resources.ResolveManager("OfficialKsu"),
                "arm64-v8a",
                Path.Combine(root, "libksud.so"));

            Assert.True(new FileInfo(library).Length > 0);
        }
        finally
        {
            Directory.Delete(root, true);
        }
    }
}
