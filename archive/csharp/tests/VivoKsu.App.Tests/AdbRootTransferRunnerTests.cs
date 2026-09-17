using FluentAssertions;
using VivoKsu.App.Models;
using VivoKsu.App.Services;

namespace VivoKsu.App.Tests;

public class AdbRootTransferRunnerTests
{
    [Fact]
    public async Task RunRootAsync_keeps_a_compound_discovery_script_as_one_su_command()
    {
        var binaryRunner = new RecordingAdbBinaryRunner();
        var runner = new AdbRootTransferRunner("adb.exe", binaryRunner);

        await runner.RunRootAsync("ADB123", "for d in /dev/block/by-name; do :; done", CancellationToken.None);

        binaryRunner.TextRequest.Should().Equal(
            "-s", "ADB123", "shell", "su", "-c", "'for d in /dev/block/by-name; do :; done'");
    }

    [Fact]
    public async Task CopyToDeviceAsync_uses_a_quoted_non_pty_root_dd_command()
    {
        var binaryRunner = new RecordingAdbBinaryRunner();
        var runner = new AdbRootTransferRunner("adb.exe", binaryRunner);

        await runner.CopyToDeviceAsync("ADB123", @"D:\images\custom.bin", "/dev/block/sda12", progress: null, CancellationToken.None);

        binaryRunner.CopyToRequest.Should().NotBeNull();
        binaryRunner.CopyToRequest!.Value.Arguments.Should().Equal(
            "-s", "ADB123", "shell", "-T", "su", "-c", "'dd of='\"'\"'/dev/block/sda12'\"'\"' bs=4M conv=fsync'");
        binaryRunner.CopyToRequest.Value.LocalPath.Should().Be(@"D:\images\custom.bin");
    }

    [Fact]
    public async Task CopyFromDeviceAsync_uses_exec_out_with_unquoted_tokens_so_adb_preserves_the_dd_command()
    {
        var binaryRunner = new RecordingAdbBinaryRunner();
        var runner = new AdbRootTransferRunner("adb.exe", binaryRunner);

        await runner.CopyFromDeviceAsync("ADB123", "/dev/block/sda12", @"D:\backups\boot.img", progress: null, CancellationToken.None);

        // adb exec-out mangles a quoted `su -c` argument: the device shell then
        // treats the whole dd command as one word and backups were tiny sh error
        // files. Passing each token separately keeps the command intact; 2>/dev/null
        // keeps dd's progress summary off the binary stdout stream.
        binaryRunner.CopyFromRequest.Should().NotBeNull();
        binaryRunner.CopyFromRequest!.Value.Arguments.Should().Equal(
            "-s", "ADB123", "exec-out", "su", "-c", "dd", "if=/dev/block/sda12", "bs=4M", "2>/dev/null");
        binaryRunner.CopyFromRequest.Value.LocalPath.Should().Be(@"D:\backups\boot.img");
    }

    [Fact]
    public async Task CopyFromDeviceAsync_rejects_a_device_path_with_shell_metacharacters()
    {
        var binaryRunner = new RecordingAdbBinaryRunner();
        var runner = new AdbRootTransferRunner("adb.exe", binaryRunner);

        var act = () => runner.CopyFromDeviceAsync("ADB123", "/dev/block/sda12;reboot", @"D:\backups\boot.img", progress: null, CancellationToken.None);

        await act.Should().ThrowAsync<InvalidOperationException>();
    }

    [Fact]
    public async Task EraseAsync_uses_blkdiscard_with_zero_fill_fallback()
    {
        var binaryRunner = new RecordingAdbBinaryRunner();
        var runner = new AdbRootTransferRunner("adb.exe", binaryRunner);

        await runner.EraseAsync("ADB123", "/dev/block/sda70", progress: null, CancellationToken.None);

        binaryRunner.TextRequest.Should().NotBeNull();
        binaryRunner.TextRequest!.Should().Equal(
            "-s", "ADB123", "shell", "su", "-c", "'blkdiscard '\"'\"'/dev/block/sda70'\"'\"' || dd if=/dev/zero of='\"'\"'/dev/block/sda70'\"'\"' bs=4M conv=fsync'");
    }

    private sealed class RecordingAdbBinaryRunner : IAdbBinaryRunner
    {
        public (IReadOnlyList<string> Arguments, string LocalPath)? CopyToRequest { get; private set; }
        public (IReadOnlyList<string> Arguments, string LocalPath)? CopyFromRequest { get; private set; }
        public IReadOnlyList<string>? TextRequest { get; private set; }

        public Task<string> RunTextAsync(string executable, IReadOnlyList<string> arguments, CancellationToken cancellationToken)
        {
            TextRequest = arguments.ToArray();
            return Task.FromResult(string.Empty);
        }

        public Task CopyFromDeviceAsync(string executable, IReadOnlyList<string> arguments, string localPath, IProgress<PartitionTransferProgress>? progress, CancellationToken cancellationToken)
        {
            CopyFromRequest = (arguments.ToArray(), localPath);
            return Task.CompletedTask;
        }

        public Task CopyToDeviceAsync(string executable, IReadOnlyList<string> arguments, string localPath, IProgress<PartitionTransferProgress>? progress, CancellationToken cancellationToken)
        {
            CopyToRequest = (arguments.ToArray(), localPath);
            return Task.CompletedTask;
        }
    }
}
