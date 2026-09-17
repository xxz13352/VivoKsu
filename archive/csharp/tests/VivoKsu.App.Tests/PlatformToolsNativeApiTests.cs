using VivoKsu.App.Services;

namespace VivoKsu.App.Tests;

public class PlatformToolsNativeApiTests
{
    [Fact]
    public void ExecutableLocator_prefers_the_vendored_platform_tools_executable()
    {
        var root = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        var directory = Path.Combine(root, "platform-tools");
        Directory.CreateDirectory(directory);
        var adb = Path.Combine(directory, "adb.exe");
        File.WriteAllText(adb, "tool");

        try
        {
            var locator = new PlatformToolsExecutableLocator(root);

            Assert.Equal(adb, locator.Resolve("adb.exe"));
            Assert.Equal("fastboot.exe", locator.Resolve("fastboot.exe"));
        }
        finally
        {
            Directory.Delete(root, true);
        }
    }

    [Fact]
    public void ListDevices_normalizes_adb_and_fastboot_output_for_the_existing_parser()
    {
        var runner = new RecordingCommandRunner()
            .Respond("adb.exe", "List of devices attached\nABC123\tdevice product:pd device:pd\n")
            .Respond("fastboot.exe", "FAST456\tfastboot\n");
        var api = new PlatformToolsNativeApi(runner, "adb.exe", "fastboot.exe");

        var output = api.ListDevices();

        Assert.Equal("ABC123\tdevice\nFAST456\tfastboot", output);
    }

    [Fact]
    public void GetVar_reads_the_value_reported_by_fastboot_on_standard_error()
    {
        var runner = new RecordingCommandRunner()
            .Respond("fastboot.exe", string.Empty, "(bootloader) current-slot: b\nFinished. Total time: 0.001s");
        var api = new PlatformToolsNativeApi(runner, "adb.exe", "fastboot.exe");

        var value = api.GetVar("FAST456", "current-slot");

        Assert.Equal("b", value);
        Assert.Equal(["-s", "FAST456", "getvar", "current-slot"], runner.Requests.Single().Arguments);
    }

    [Fact]
    public void Flash_forwards_partition_and_image_as_distinct_arguments()
    {
        var runner = new RecordingCommandRunner().Respond("fastboot.exe", string.Empty);
        var api = new PlatformToolsNativeApi(runner, "adb.exe", "fastboot.exe");

        api.Flash("FAST456", "init_boot", "C:\\images\\init boot.img");

        Assert.Equal(["-s", "FAST456", "flash", "init_boot", "C:\\images\\init boot.img"], runner.Requests.Single().Arguments);
    }

    [Fact]
    public void SetActive_runs_fastboot_set_active_for_the_requested_serial()
    {
        var runner = new RecordingCommandRunner().Respond("fastboot.exe", string.Empty);
        var api = new PlatformToolsNativeApi(runner, "adb.exe", "fastboot.exe");

        api.SetActive("FAST456", "b");

        var request = Assert.Single(runner.Requests);
        Assert.Equal("fastboot.exe", request.Executable);
        Assert.Equal(["-s", "FAST456", "set_active", "b"], request.Arguments);
    }

    [Fact]
    public void Erase_runs_fastboot_erase_for_the_requested_partition()
    {
        var runner = new RecordingCommandRunner().Respond("fastboot.exe", string.Empty);
        var api = new PlatformToolsNativeApi(runner, "adb.exe", "fastboot.exe");

        api.Erase("FAST456", "metadata");

        Assert.Equal(["-s", "FAST456", "erase", "metadata"], runner.Requests.Single().Arguments);
    }

    [Fact]
    public void Fetch_runs_fastboot_fetch_with_a_distinct_output_path()
    {
        var runner = new RecordingCommandRunner().Respond("fastboot.exe", string.Empty);
        var api = new PlatformToolsNativeApi(runner, "adb.exe", "fastboot.exe");

        api.Fetch("FAST456", "boot_a", @"D:\backups\boot a.img");

        Assert.Equal(["-s", "FAST456", "fetch", "boot_a", @"D:\backups\boot a.img"], runner.Requests.Single().Arguments);
    }

    private sealed class RecordingCommandRunner : IPlatformToolsCommandRunner
    {
        private readonly Dictionary<string, PlatformToolsCommandResult> responses = new(StringComparer.OrdinalIgnoreCase);

        public List<(string Executable, IReadOnlyList<string> Arguments)> Requests { get; } = [];

        public RecordingCommandRunner Respond(string executable, string output, string error = "", int exitCode = 0)
        {
            responses[executable] = new PlatformToolsCommandResult(exitCode, output, error);
            return this;
        }

        public PlatformToolsCommandResult Run(string executable, IReadOnlyList<string> arguments, int timeoutMilliseconds = 15000)
        {
            Requests.Add((executable, arguments.ToArray()));
            return responses[executable];
        }
    }
}
