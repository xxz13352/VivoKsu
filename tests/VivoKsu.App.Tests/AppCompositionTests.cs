using FluentAssertions;
using VivoKsu.App.Models;
using VivoKsu.App.Services;
using VivoKsu.App.ViewModels;

namespace VivoKsu.App.Tests;

public class AppCompositionTests
{
    [Fact]
    public void CreateForTesting_uses_one_shared_session_log_and_coordinator()
    {
        var composition = AppComposition.CreateForTesting(new EmptyNativeApi(), new FakeProcessRunner());

        Assert.Same(composition.Session, composition.MainViewModel.DeviceSession);
        Assert.Same(composition.LogService.Entries, composition.MainViewModel.Logs.Entries);
        Assert.Same(composition.Coordinator, composition.MainViewModel.Coordinator);
        Assert.Same(composition.Coordinator, composition.MainViewModel.Root.Coordinator);
    }

    [Fact]
    public void Composition_exposes_heartbeat_and_online_and_wires_online_into_main()
    {
        var composition = AppComposition.CreateForTesting(new EmptyNativeApi(), new FakeProcessRunner());

        Assert.NotNull(composition.Heartbeat);
        Assert.NotNull(composition.Online);
        Assert.Same(composition.Online, composition.MainViewModel.Online);
    }

    [Fact]
    public async Task StopAsync_stops_heartbeat_and_online_without_throwing_when_session_was_never_started()
    {
        var composition = AppComposition.CreateForTesting(new EmptyNativeApi(), new FakeProcessRunner());

        await composition.StopAsync();

        composition.Heartbeat.IsRunning.Should().BeFalse();
    }

    [Fact]
    public async Task Logout_command_stops_the_composition_and_raises_logout_requested()
    {
        var composition = AppComposition.CreateForTesting(new EmptyNativeApi(), new FakeProcessRunner());
        var logoutRaised = false;
        composition.LogoutRequested += (_, _) => logoutRaised = true;

        await composition.MainViewModel.LogoutCommand.ExecuteAsync(null);

        Assert.True(logoutRaised);
        Assert.False(composition.Heartbeat.IsRunning);
    }

    private sealed class EmptyNativeApi : IFastbootRsNativeApi
    {
        public string ListDevices() => string.Empty;
        public string Shell(string? serial, string command, int timeoutMilliseconds = 15000) => string.Empty;
        public string GetVar(string? serial, string variable) => string.Empty;
        public void Reboot(string? serial, string target) { }
        public void FastbootReboot(string? serial, string? target) { }
        public void Push(string? serial, string localPath, string remotePath) { }
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
