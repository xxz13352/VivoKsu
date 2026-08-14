using VivoKsu.App.Models;
using VivoKsu.App.Services;
using VivoKsu.App.ViewModels;

namespace VivoKsu.App.Tests;

public class MirrorServiceTests
{
    [Fact]
    public async Task ReconcileAsync_starts_scrcpy_for_an_adb_device_when_auto_mirror_is_enabled()
    {
        var runner = new FakeProcessRunner();
        var service = new MirrorService(runner, new OperationLogService(), new AvailableScrcpyLocator());
        var session = new DeviceSessionViewModel();
        session.ApplyDevice(new DeviceSnapshot(DeviceConnectionState.AdbConnected, "RF8", "ADB 已连接"));
        service.AutoMirrorEnabled = true;

        await service.ReconcileAsync(session, CancellationToken.None);

        Assert.Equal("RF8", runner.LastSerial);
        Assert.True(service.IsMirroring);
    }

    [Fact]
    public async Task StartAsync_points_scrcpy_at_platform_tools_adb_via_the_adb_environment_variable()
    {
        // scrcpy v4.0 移除了 --adb-path 选项(遇到它会以 "unknown option" 立即退出),
        // adb 路径必须改为通过 ADB 环境变量注入,且参数里不再出现 --adb-path。
        var adbDirectory = Path.Combine(AppContext.BaseDirectory, "platform-tools");
        var adbPath = Path.Combine(adbDirectory, "adb.exe");
        Directory.CreateDirectory(adbDirectory);
        try
        {
            File.WriteAllBytes(adbPath, new byte[] { 0 });
            var runner = new FakeProcessRunner();
            var service = new MirrorService(runner, new OperationLogService(), new AvailableScrcpyLocator());
            var session = new DeviceSessionViewModel();
            session.ApplyDevice(new DeviceSnapshot(DeviceConnectionState.AdbConnected, "RF8", "ADB 已连接"));

            await service.StartAsync(session, CancellationToken.None);

            Assert.DoesNotContain(runner.LastArguments!, argument => argument == "--adb-path");
            Assert.Equal(adbPath, runner.LastEnvironment!["ADB"]);
        }
        finally
        {
            Directory.Delete(adbDirectory, recursive: true);
        }
    }

    [Fact]
    public async Task StopAsync_suppresses_auto_restart_after_a_deliberate_stop()
    {
        var runner = new FakeProcessRunner();
        var service = new MirrorService(runner, new OperationLogService(), new AvailableScrcpyLocator());
        var session = new DeviceSessionViewModel();
        session.ApplyDevice(new DeviceSnapshot(DeviceConnectionState.AdbConnected, "RF8", "ADB 已连接"));
        service.AutoMirrorEnabled = true;
        await service.ReconcileAsync(session, CancellationToken.None);

        await service.StopAsync();
        await service.ReconcileAsync(session, CancellationToken.None);

        Assert.Equal(1, runner.StartCount);
        Assert.False(service.IsMirroring);
    }

    [Fact]
    public async Task ReconcileAsync_does_not_start_a_process_when_scrcpy_is_missing()
    {
        var runner = new FakeProcessRunner();
        var logs = new OperationLogService();
        var service = new MirrorService(runner, logs, new MissingScrcpyLocator());
        var session = new DeviceSessionViewModel();
        session.ApplyDevice(new DeviceSnapshot(DeviceConnectionState.AdbConnected, "RF8", "ADB 已连接"));
        service.AutoMirrorEnabled = true;

        await service.ReconcileAsync(session, CancellationToken.None);

        Assert.Equal(0, runner.StartCount);
        Assert.Contains(logs.Entries, entry => entry.Level == OperationLogLevel.Warning && entry.Message.Contains("未检测到 scrcpy.exe"));
    }

    [Fact]
    public async Task MirrorViewModel_updates_running_state_when_the_mirror_process_exits()
    {
        var runner = new FakeProcessRunner();
        var service = new MirrorService(runner, new OperationLogService(), new AvailableScrcpyLocator());
        var session = new DeviceSessionViewModel();
        session.ApplyDevice(new DeviceSnapshot(DeviceConnectionState.AdbConnected, "RF8", "ADB 已连接"));
        var viewModel = new MirrorViewModel(session, service);

        await viewModel.StartMirrorCommand.ExecuteAsync(null);
        runner.LastProcess!.Exit();

        Assert.False(viewModel.IsMirroring);
        Assert.Equal("投屏未启动", viewModel.MirrorStatusText);
    }

    [Fact]
    public async Task Process_exit_from_a_worker_thread_is_marshaled_to_the_creation_context()
    {
        var previousContext = SynchronizationContext.Current;
        var context = new RecordingSynchronizationContext();
        SynchronizationContext.SetSynchronizationContext(context);
        try
        {
            var runner = new FakeProcessRunner();
            var service = new MirrorService(runner, new OperationLogService(), new AvailableScrcpyLocator());
            var session = new DeviceSessionViewModel();
            session.ApplyDevice(new DeviceSnapshot(DeviceConnectionState.AdbConnected, "RF8", "ADB 已连接"));
            await service.StartAsync(session, CancellationToken.None);

            var exitTask = Task.Run(() => runner.LastProcess!.Exit());
            SynchronizationContext.SetSynchronizationContext(previousContext);
            await exitTask;

            Assert.Equal(1, context.PostCount);
        }
        finally
        {
            SynchronizationContext.SetSynchronizationContext(previousContext);
        }
    }

    [Fact]
    public void MirrorViewModel_configures_an_external_scrcpy_tool_and_refreshes_availability()
    {
        var root = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        var executable = Path.Combine(root, "scrcpy.exe");
        Directory.CreateDirectory(root);
        File.WriteAllText(executable, "tool");
        var service = new MirrorService(new FakeProcessRunner(), new OperationLogService(), new ScrcpyToolLocator(root, []));
        var viewModel = new MirrorViewModel(new DeviceSessionViewModel(), service);

        try
        {
            var configure = typeof(MirrorViewModel).GetMethod("ConfigureScrcpyTool");
            Assert.NotNull(configure);
            configure!.Invoke(viewModel, [executable]);

            Assert.True(viewModel.IsScreenCastToolAvailable);
            Assert.Equal("scrcpy 已就绪（外部工具）", viewModel.ScreenCastToolStatus);
        }
        finally
        {
            Directory.Delete(root, true);
        }
    }

    [Fact]
    public void MirrorViewModel_persists_an_external_scrcpy_tool_path()
    {
        var root = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        var executable = Path.Combine(root, "scrcpy.exe");
        var settingsPath = Path.Combine(root, "settings.json");
        Directory.CreateDirectory(root);
        File.WriteAllText(executable, "tool");
        var preferences = new ToolPathPreferences(settingsPath);
        var service = new MirrorService(new FakeProcessRunner(), new OperationLogService(), new ScrcpyToolLocator(root, []));
        var viewModel = new MirrorViewModel(new DeviceSessionViewModel(), service, preferences);

        try
        {
            viewModel.ConfigureScrcpyTool(executable);

            Assert.Equal(executable, new ToolPathPreferences(settingsPath).ScrcpyPath);
        }
        finally
        {
            Directory.Delete(root, true);
        }
    }

    [Fact]
    public void MirrorViewModel_restores_a_saved_scrcpy_tool_path()
    {
        var root = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        var executable = Path.Combine(root, "scrcpy.exe");
        var settingsPath = Path.Combine(root, "settings.json");
        Directory.CreateDirectory(root);
        File.WriteAllText(executable, "tool");
        var preferences = new ToolPathPreferences(settingsPath);
        preferences.SaveScrcpyPath(executable);

        try
        {
            var service = new MirrorService(new FakeProcessRunner(), new OperationLogService(), new ScrcpyToolLocator(root, []));
            var viewModel = new MirrorViewModel(new DeviceSessionViewModel(), service, new ToolPathPreferences(settingsPath));

            Assert.True(viewModel.IsScreenCastToolAvailable);
            Assert.Equal("scrcpy 已就绪（外部工具）", viewModel.ScreenCastToolStatus);
        }
        finally
        {
            Directory.Delete(root, true);
        }
    }

    private sealed class FakeProcessRunner : IProcessRunner
    {
        public int StartCount { get; private set; }
        public string? LastSerial { get; private set; }
        public IReadOnlyList<string>? LastArguments { get; private set; }
        public IReadOnlyDictionary<string, string>? LastEnvironment { get; private set; }
        public FakeRunningProcess? LastProcess { get; private set; }

        public IRunningProcess Start(string executable, IReadOnlyList<string> arguments, IReadOnlyDictionary<string, string>? environment = null)
        {
            StartCount++;
            LastArguments = arguments;
            LastEnvironment = environment;
            var serialIndex = Array.FindIndex(arguments.ToArray(), static value => value == "--serial");
            LastSerial = serialIndex >= 0 && serialIndex + 1 < arguments.Count ? arguments[serialIndex + 1] : null;
            LastProcess = new FakeRunningProcess();
            return LastProcess;
        }
    }

    private sealed class FakeRunningProcess : IRunningProcess
    {
        public bool HasExited { get; private set; }
        public event EventHandler? Exited;
        public void Stop() { HasExited = true; Exited?.Invoke(this, EventArgs.Empty); }
        public void Exit() { HasExited = true; Exited?.Invoke(this, EventArgs.Empty); }
        public void Dispose() { }
    }

    private sealed class RecordingSynchronizationContext : SynchronizationContext
    {
        public int PostCount { get; private set; }

        public override void Post(SendOrPostCallback callback, object? state) => PostCount++;
    }

    private sealed class AvailableScrcpyLocator : IScrcpyToolLocator
    {
        public bool IsAvailable => true;
        public string? ExecutablePath => "scrcpy.exe";
        public string StatusMessage => "scrcpy 已就绪";
        public void ConfigureToolPath(string toolPath) { }
    }

    private sealed class MissingScrcpyLocator : IScrcpyToolLocator
    {
        public bool IsAvailable => false;
        public string? ExecutablePath => null;
        public string StatusMessage => "未检测到 scrcpy.exe";
        public void ConfigureToolPath(string toolPath) => throw new FileNotFoundException();
    }
}
