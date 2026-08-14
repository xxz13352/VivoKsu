using VivoKsu.App.Models;
using VivoKsu.App.Services;
using VivoKsu.App.ViewModels;

namespace VivoKsu.App.Tests;

public class FileManagerViewModelTests
{
    [Fact]
    public void File_manager_starts_in_the_sdcard_root()
    {
        var viewModel = CreateViewModel(new DeviceSessionViewModel(), new EmptyNativeApi());

        Assert.Equal("/sdcard", viewModel.CurrentRemotePath);
    }

    [Fact]
    public void GoUpRemoteCommand_is_disabled_at_the_device_root()
    {
        var viewModel = CreateViewModel(new DeviceSessionViewModel(), new EmptyNativeApi());
        viewModel.CurrentRemotePath = "/";

        Assert.False(viewModel.GoUpRemoteCommand.CanExecute(null));
    }

    [Fact]
    public void RequestDeleteCommand_shows_a_confirmation_for_the_selected_remote_file()
    {
        var session = new DeviceSessionViewModel();
        session.ApplyDevice(new DeviceSnapshot(DeviceConnectionState.AdbConnected, "ADB001", "ADB 已连接"));
        var viewModel = CreateViewModel(session, new EmptyNativeApi());
        viewModel.SelectedRemote = new DeviceFileEntry("update.zip", "/sdcard/Download/update.zip", false, 1024);

        viewModel.RequestDeleteCommand.Execute(null);

        Assert.True(viewModel.IsDeleteConfirmationVisible);
    }

    [Fact]
    public async Task ConfirmDelete_deletes_the_entry_captured_when_confirmation_was_requested()
    {
        var session = new DeviceSessionViewModel();
        session.ApplyDevice(new DeviceSnapshot(DeviceConnectionState.AdbConnected, "ADB001", "ADB 已连接"));
        var native = new RecordingShellNativeApi();
        var viewModel = CreateViewModel(session, native);
        var requested = new DeviceFileEntry("first.bin", "/sdcard/first.bin", false, 10);
        var laterSelection = new DeviceFileEntry("second.bin", "/sdcard/second.bin", false, 20);
        viewModel.SelectedRemote = requested;

        viewModel.RequestDeleteCommand.Execute(null);
        viewModel.SelectedRemote = laterSelection;
        await viewModel.ConfirmDeleteCommand.ExecuteAsync(null);

        Assert.Contains("rm -rf -- '/sdcard/first.bin'", native.ShellCommands);
        Assert.DoesNotContain("rm -rf -- '/sdcard/second.bin'", native.ShellCommands);
    }

    [Fact]
    public void ConfirmDeleteCommand_remains_available_for_the_captured_entry_when_selection_is_cleared()
    {
        var session = new DeviceSessionViewModel();
        session.ApplyDevice(new DeviceSnapshot(DeviceConnectionState.AdbConnected, "ADB001", "ADB 已连接"));
        var viewModel = CreateViewModel(session, new EmptyNativeApi());
        viewModel.SelectedRemote = new DeviceFileEntry("update.zip", "/sdcard/update.zip", false, 1024);

        viewModel.RequestDeleteCommand.Execute(null);
        viewModel.SelectedRemote = null;

        Assert.True(viewModel.ConfirmDeleteCommand.CanExecute(null));
    }

    [Fact]
    public void Remote_file_state_is_reset_when_the_connected_device_changes()
    {
        var session = new DeviceSessionViewModel();
        session.ApplyDevice(new DeviceSnapshot(DeviceConnectionState.AdbConnected, "ADB001", "ADB 已连接"));
        var viewModel = CreateViewModel(session, new EmptyNativeApi());
        var entry = new DeviceFileEntry("update.zip", "/data/local/tmp/update.zip", false, 1024);
        viewModel.CurrentRemotePath = "/data/local/tmp";
        viewModel.RemoteFiles.Add(entry);
        viewModel.SelectedRemote = entry;
        viewModel.RequestDeleteCommand.Execute(null);

        session.ApplyDevice(new DeviceSnapshot(DeviceConnectionState.AdbConnected, "ADB002", "ADB 已连接"));

        Assert.False(viewModel.IsDeleteConfirmationVisible);
        Assert.False(viewModel.ConfirmDeleteCommand.CanExecute(null));
        Assert.Null(viewModel.SelectedRemote);
        Assert.Empty(viewModel.RemoteFiles);
        Assert.Equal("/sdcard", viewModel.CurrentRemotePath);
    }

    [Fact]
    public void UploadCommand_is_available_with_an_adb_device_without_preselecting_a_local_file()
    {
        var session = new DeviceSessionViewModel();
        session.ApplyDevice(new DeviceSnapshot(DeviceConnectionState.AdbConnected, "ADB001", "ADB 已连接"));
        var viewModel = CreateViewModel(session, new EmptyNativeApi());

        Assert.True(viewModel.UploadCommand.CanExecute(null));
    }

    [Fact]
    public void UploadCommand_rechecks_availability_when_the_device_connects_after_page_creation()
    {
        var session = new DeviceSessionViewModel();
        var viewModel = CreateViewModel(session, new EmptyNativeApi());

        Assert.False(viewModel.UploadCommand.CanExecute(null));

        session.ApplyDevice(new DeviceSnapshot(DeviceConnectionState.AdbConnected, "ADB001", "ADB 已连接"));

        Assert.True(viewModel.UploadCommand.CanExecute(null));
    }

    [Fact]
    public async Task Upload_uses_the_shared_coordinator_for_the_full_transfer()
    {
        var directory = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(directory);
        var localPath = Path.Combine(directory, "payload.bin");
        await File.WriteAllBytesAsync(localPath, "payload"u8.ToArray());
        using var pushStarted = new ManualResetEventSlim();
        using var releasePush = new ManualResetEventSlim();
        var native = new BlockingPushNativeApi(pushStarted, releasePush);
        var composition = AppComposition.CreateForTesting(native, new FakeProcessRunner());
        composition.Session.ApplyDevice(new DeviceSnapshot(DeviceConnectionState.AdbConnected, "ADB001", "ADB 已连接"));
        composition.MainViewModel.FileManager.SelectedLocal = new DeviceFileEntry("payload.bin", localPath, false, 7);

        try
        {
            var upload = composition.MainViewModel.FileManager.UploadCommand.ExecuteAsync(null);
            Assert.True(pushStarted.Wait(TimeSpan.FromSeconds(5)));
            Assert.True(composition.Coordinator.IsBusy);

            releasePush.Set();
            await upload;

            Assert.False(composition.Coordinator.IsBusy);
            Assert.Equal(OperationKind.Completed, composition.Session.OperationKind);
        }
        finally
        {
            releasePush.Set();
            await composition.StopAsync();
            Directory.Delete(directory, true);
        }
    }

    [Fact]
    public async Task Refresh_preserves_the_previous_local_listing_when_local_enumeration_fails()
    {
        var session = new DeviceSessionViewModel();
        session.ApplyDevice(new DeviceSnapshot(DeviceConnectionState.AdbConnected, "ADB001", "ADB 已连接"));
        var viewModel = CreateViewModel(session, new EmptyNativeApi());
        var existing = new DeviceFileEntry("keep.bin", "C:\\existing\\keep.bin", false, 4);
        viewModel.LocalFiles.Add(existing);
        viewModel.CurrentLocalPath = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));

        await viewModel.RefreshCommand.ExecuteAsync(null);

        Assert.Contains(existing, viewModel.LocalFiles);
    }

    [Fact]
    public async Task Upload_remains_successful_when_post_upload_directory_refresh_fails()
    {
        var directory = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(directory);
        var localPath = Path.Combine(directory, "payload.bin");
        await File.WriteAllBytesAsync(localPath, "payload"u8.ToArray());
        var native = new PushThenFailListingNativeApi();
        var composition = AppComposition.CreateForTesting(native, new FakeProcessRunner());
        composition.Session.ApplyDevice(new DeviceSnapshot(DeviceConnectionState.AdbConnected, "ADB001", "ADB 已连接"));
        composition.MainViewModel.FileManager.SelectedLocal = new DeviceFileEntry("payload.bin", localPath, false, 7);

        try
        {
            await composition.MainViewModel.FileManager.UploadCommand.ExecuteAsync(null);

            Assert.True(native.PushCalled);
            Assert.Equal(OperationKind.Completed, composition.Session.OperationKind);
            Assert.Contains(composition.LogService.Entries, entry =>
                entry.Level == OperationLogLevel.Warning && entry.Message.Contains("目录刷新失败", StringComparison.Ordinal));
        }
        finally
        {
            await composition.StopAsync();
            Directory.Delete(directory, true);
        }
    }

    [Fact]
    public void NavigateLocalDirectory_updates_the_current_path_and_reloads_the_listing()
    {
        var root = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        var child = Path.Combine(root, "images");
        Directory.CreateDirectory(child);
        File.WriteAllText(Path.Combine(child, "boot.img"), "image");

        try
        {
            var viewModel = CreateViewModel(new DeviceSessionViewModel(), new EmptyNativeApi());
            viewModel.CurrentLocalPath = root;
            var entry = new DeviceFileEntry("images", child, true, 0);
            var navigate = viewModel.GetType().GetMethod("NavigateLocal");

            Assert.NotNull(navigate);
            navigate!.Invoke(viewModel, [entry]);

            Assert.Equal(child, viewModel.CurrentLocalPath);
            Assert.Contains(viewModel.LocalFiles, file => file.Name == "boot.img");
        }
        finally
        {
            Directory.Delete(root, true);
        }
    }

    [Fact]
    public async Task NavigateRemoteDirectory_updates_the_path_and_queries_the_target_directory()
    {
        var session = new DeviceSessionViewModel();
        session.ApplyDevice(new DeviceSnapshot(DeviceConnectionState.AdbConnected, "RF8", "设备已连接"));
        var native = new DirectoryNativeApi();
        var viewModel = CreateViewModel(session, native);
        var entry = new DeviceFileEntry("Camera", "/sdcard/Download/Camera", true, 0);
        var navigate = viewModel.GetType().GetMethod("NavigateRemoteAsync");

        Assert.NotNull(navigate);
        await (Task)navigate!.Invoke(viewModel, [entry])!;

        Assert.Equal("/sdcard/Download/Camera", viewModel.CurrentRemotePath);
        Assert.Equal("ls -laL -- '/sdcard/Download/Camera/'", native.LastShellCommand);
        Assert.Contains(viewModel.RemoteFiles, file => file.Name == "IMG_0001.jpg");
    }

    [Fact]
    public async Task NavigateRemoteDirectory_restores_the_previous_path_when_listing_fails()
    {
        var session = new DeviceSessionViewModel();
        session.ApplyDevice(new DeviceSnapshot(DeviceConnectionState.AdbConnected, "RF8", "设备已连接"));
        var viewModel = CreateViewModel(session, new FailingDirectoryNativeApi());
        var entry = new DeviceFileEntry("Restricted", "/sdcard/Restricted", true, 0);

        await viewModel.NavigateRemoteAsync(entry);

        Assert.Equal("/sdcard", viewModel.CurrentRemotePath);
    }

    private static FileManagerViewModel CreateViewModel(DeviceSessionViewModel session, IFastbootRsNativeApi native)
    {
        var logs = new OperationLogService();
        return new FileManagerViewModel(session, new AdbFileService(new FastbootRsBackend(native), logs), logs);
    }

    private class EmptyNativeApi : IFastbootRsNativeApi
    {
        public string ListDevices() => string.Empty;
        public virtual string Shell(string? serial, string command) => string.Empty;
        public string GetVar(string? serial, string variable) => string.Empty;
        public void Reboot(string? serial, string target) { }
        public virtual void Push(string? serial, string localPath, string remotePath) { }
        public long Pull(string? serial, string remotePath, string localPath) => 0;
        public string Install(string? serial, string apkPath, bool replace) => string.Empty;
        public void Flash(string? serial, string partition, string imagePath) { }
    }

    private sealed class DirectoryNativeApi : IFastbootRsNativeApi
    {
        public string? LastShellCommand { get; private set; }
        public string ListDevices() => "RF8\tdevice\n";
        public string Shell(string? serial, string command)
        {
            LastShellCommand = command;
            return "-rw-rw---- 1 u0_a123 media_rw 2048 2026-08-10 11:21 IMG_0001.jpg\n";
        }
        public string GetVar(string? serial, string variable) => string.Empty;
        public void Reboot(string? serial, string target) { }
        public void Push(string? serial, string localPath, string remotePath) { }
        public long Pull(string? serial, string remotePath, string localPath) => 0;
        public string Install(string? serial, string apkPath, bool replace) => string.Empty;
        public void Flash(string? serial, string partition, string imagePath) { }
    }

    private sealed class FailingDirectoryNativeApi : EmptyNativeApi
    {
        public override string Shell(string? serial, string command) =>
            throw new InvalidOperationException("permission denied");
    }

    private sealed class RecordingShellNativeApi : EmptyNativeApi
    {
        public List<string> ShellCommands { get; } = [];

        public override string Shell(string? serial, string command)
        {
            ShellCommands.Add(command);
            return string.Empty;
        }
    }

    private sealed class PushThenFailListingNativeApi : EmptyNativeApi
    {
        public bool PushCalled { get; private set; }

        public override string Shell(string? serial, string command) =>
            throw new InvalidOperationException("device disconnected after upload");

        public override void Push(string? serial, string localPath, string remotePath)
        {
            PushCalled = true;
        }
    }

    private sealed class BlockingPushNativeApi(
        ManualResetEventSlim pushStarted,
        ManualResetEventSlim releasePush) : IFastbootRsNativeApi
    {
        public string ListDevices() => string.Empty;
        public string Shell(string? serial, string command) => string.Empty;
        public string GetVar(string? serial, string variable) => string.Empty;
        public void Reboot(string? serial, string target) { }
        public void Push(string? serial, string localPath, string remotePath)
        {
            pushStarted.Set();
            releasePush.Wait(TimeSpan.FromSeconds(10));
        }
        public long Pull(string? serial, string remotePath, string localPath) => 0;
        public string Install(string? serial, string apkPath, bool replace) => string.Empty;
        public void Flash(string? serial, string partition, string imagePath) { }
    }

    private sealed class FakeProcessRunner : IProcessRunner
    {
        public IRunningProcess Start(string executable, IReadOnlyList<string> arguments, IReadOnlyDictionary<string, string>? environment = null) => new FakeRunningProcess();
    }

    private sealed class FakeRunningProcess : IRunningProcess
    {
        public bool HasExited => true;
        public event EventHandler? Exited;
        public void Stop() => Exited?.Invoke(this, EventArgs.Empty);
        public void Dispose() { }
    }
}
