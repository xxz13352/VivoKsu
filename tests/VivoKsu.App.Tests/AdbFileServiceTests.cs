using VivoKsu.App.Models;
using VivoKsu.App.Services;

namespace VivoKsu.App.Tests;

public class AdbFileServiceTests
{
    [Fact]
    public async Task ListRemoteAsync_parses_files_and_directories_from_adb_ls_output()
    {
        var native = new FileNativeApi
        {
            ShellResult = "drwxrwx--x 2 u0_a123 media_rw 4096 2026-08-10 11:20 Camera\n-rw-rw---- 1 u0_a123 media_rw 2048 2026-08-10 11:21 update.zip\n"
        };
        var service = new AdbFileService(new FastbootRsBackend(native), new OperationLogService());

        var entries = await service.ListRemoteAsync("RF8", "/sdcard/Download", CancellationToken.None);

        Assert.Collection(entries,
            directory => { Assert.Equal("Camera", directory.Name); Assert.True(directory.IsDirectory); },
            file => { Assert.Equal("update.zip", file.Name); Assert.False(file.IsDirectory); Assert.Equal(2048, file.SizeBytes); });
    }

    [Fact]
    public async Task ListRemoteAsync_preserves_the_device_root_path()
    {
        var native = new FileNativeApi
        {
            ShellResult = "-rw-r--r-- 1 root root 12 2026-08-10 11:21 default.prop\n"
        };
        var service = new AdbFileService(new FastbootRsBackend(native), new OperationLogService());

        var entries = await service.ListRemoteAsync("RF8", "/", CancellationToken.None);

        Assert.Equal("ls -laL -- '/'", native.LastShellCommand);
        Assert.Equal("/default.prop", Assert.Single(entries).FullPath);
    }

    [Fact]
    public async Task ListRemoteAsync_quotes_directory_names_containing_apostrophes()
    {
        var native = new FileNativeApi();
        var service = new AdbFileService(new FastbootRsBackend(native), new OperationLogService());

        await service.ListRemoteAsync("RF8", "/sdcard/O'Brien", CancellationToken.None);

        Assert.Equal("ls -laL -- '/sdcard/O'\\''Brien/'", native.LastShellCommand);
    }

    [Fact]
    public async Task ListRemoteAsync_follows_the_sdcard_link_and_parses_setgid_directories()
    {
        var native = new FileNativeApi
        {
            ShellResult = "drwxrws--- 24 u0_a313 media_rw 24576 2026-08-11 00:28 Download\n"
        };
        var service = new AdbFileService(new FastbootRsBackend(native), new OperationLogService());

        var entries = await service.ListRemoteAsync("RF8", "/sdcard", CancellationToken.None);

        Assert.Equal("ls -laL -- '/sdcard/'", native.LastShellCommand);
        var directory = Assert.Single(entries);
        Assert.Equal("Download", directory.Name);
        Assert.True(directory.IsDirectory);
    }

    [Fact]
    public async Task InstallApkAsync_rejects_a_non_apk_file_without_touching_adb()
    {
        var native = new FileNativeApi();
        var service = new AdbFileService(new FastbootRsBackend(native), new OperationLogService());

        await Assert.ThrowsAsync<ArgumentException>(() => service.InstallApkAsync("RF8", "C:\\images\\boot.img", CancellationToken.None));

        Assert.False(native.InstallCalled);
    }

    [Theory]
    [InlineData("..\\outside.bin")]
    [InlineData("C:\\outside.bin")]
    [InlineData("folder/file.bin")]
    [InlineData("bad:name.bin")]
    [InlineData("CON.img")]
    [InlineData("trailingdot.")]
    [InlineData("trailingspace ")]
    public async Task DownloadAsync_rejects_names_that_are_not_safe_Windows_file_names(string remoteName)
    {
        var localDirectory = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        var native = new FileNativeApi();
        var service = new AdbFileService(new FastbootRsBackend(native), new OperationLogService());
        var remoteFile = new DeviceFileEntry(remoteName, $"/sdcard/{remoteName}", false, 1);

        await Assert.ThrowsAsync<ArgumentException>(() =>
            service.DownloadAsync("RF8", remoteFile, localDirectory, CancellationToken.None));

        Assert.False(native.PullCalled);
    }

    private sealed class FileNativeApi : IFastbootRsNativeApi
    {
        public string ShellResult { get; set; } = string.Empty;
        public bool InstallCalled { get; private set; }
        public bool PullCalled { get; private set; }
        public string? LastShellCommand { get; private set; }
        public string ListDevices() => string.Empty;
        public string Shell(string? serial, string command)
        {
            LastShellCommand = command;
            return ShellResult;
        }
        public string GetVar(string? serial, string variable) => string.Empty;
        public void Reboot(string? serial, string target) { }
        public void Push(string? serial, string localPath, string remotePath) { }
        public long Pull(string? serial, string remotePath, string localPath) { PullCalled = true; return 0; }
        public string Install(string? serial, string apkPath, bool replace) { InstallCalled = true; return "Success"; }
        public void Flash(string? serial, string partition, string imagePath) { }
    }
}
