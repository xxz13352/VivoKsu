using VivoKsu.App.Models;
using VivoKsu.App.Services;
using VivoKsu.App.ViewModels;

namespace VivoKsu.App.Tests;

public class ResourceDownloadViewModelTests
{
    private static ResourceDownloadViewModel CreateVm() =>
        new(new ScrcpyProvisioningService(), new PayloadDumperProvisioner(), new VivoRootResourceService(AppContext.BaseDirectory));

    [Fact]
    public void Detect_builds_the_four_resources()
    {
        var vm = CreateVm();
        vm.Detect();

        // scrcpy / KSU / KernelSU / payload_dumper 四项(就绪判定均为本地 File.Exists,无网络)。
        Assert.Equal(4, vm.Items.Count);
    }

    [Fact]
    public void Missing_items_are_selected_and_installed_items_are_not()
    {
        var vm = CreateVm();
        vm.AddItem("a", "A", "1 MB", isInstalled: true, installer: (_, _) => Task.CompletedTask);
        vm.AddItem("b", "B", "1 MB", isInstalled: false, installer: (_, _) => Task.CompletedTask);

        Assert.Equal(1, vm.MissingCount);
        Assert.Equal(1, vm.SelectedCount);
        Assert.False(vm.Items[0].IsSelected);
        Assert.True(vm.Items[1].IsSelected);
    }

    [Fact]
    public async Task Install_runs_selected_items_in_parallel_and_marks_ready()
    {
        var vm = CreateVm();
        var started = new List<string>();
        var gate = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        vm.AddItem("a", "A", "1 MB", false, (ct, _) =>
        {
            started.Add("a");
            return gate.Task;
        });
        vm.AddItem("b", "B", "1 MB", false, (ct, _) =>
        {
            started.Add("b");
            return Task.CompletedTask;
        });

        var run = vm.InstallCoreForTestingAsync(vm.Items[0], vm.Items[1]);
        // 两项都进入 installer(证明并行:第二个无需等第一个完成)。
        await WaitUntilAsync(() => started.Count == 2);
        gate.SetResult();
        await run;

        Assert.Equal(ResourceDownloadStatus.Ready, vm.Items[0].Status);
        Assert.Equal(ResourceDownloadStatus.Ready, vm.Items[1].Status);
        Assert.False(vm.IsDownloading);
        Assert.Equal(0, vm.SelectedCount);
    }

    [Fact]
    public async Task Failure_is_isolated_other_items_still_succeed()
    {
        var vm = CreateVm();
        vm.AddItem("bad", "失败项", "1 MB", false, (ct, _) => throw new InvalidOperationException("boom"));
        vm.AddItem("ok", "成功项", "1 MB", false, (ct, _) => Task.CompletedTask);

        await vm.InstallCoreForTestingAsync(vm.Items[0], vm.Items[1]);

        Assert.Equal(ResourceDownloadStatus.Failed, vm.Items[0].Status);
        Assert.Equal(ResourceDownloadStatus.Ready, vm.Items[1].Status);
        Assert.Equal(1, vm.FailedCount);
    }

    [Fact]
    public async Task Cancel_marks_in_flight_items_as_skipped()
    {
        var vm = CreateVm();
        var gate = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        vm.AddItem("slow", "慢项", "1 MB", false, async (ct, _) =>
        {
            await gate.Task;
            ct.ThrowIfCancellationRequested();
        });

        var run = vm.InstallCoreForTestingAsync(vm.Items[0]);
        await WaitUntilAsync(() => vm.Items[0].Status == ResourceDownloadStatus.Downloading);
        vm.Cancel();
        gate.SetResult();
        await run;

        Assert.Equal(ResourceDownloadStatus.Skipped, vm.Items[0].Status);
        Assert.False(vm.IsDownloading);
    }

    [Fact]
    public void ApplyProgress_maps_bytes_to_percent_speed_and_indeterminate()
    {
        var vm = CreateVm();
        vm.AddItem("p", "P", "1 MB", false, (_, _) => Task.CompletedTask);
        var item = vm.Items[0];

        item.ApplyProgress(new DownloadProgress(1_572_864, 3_145_728, 1_048_576));
        Assert.Equal(0.5, item.Progress, precision: 3);
        Assert.False(item.IsIndeterminate);
        Assert.Contains("1.5 MB", item.ProgressText);
        Assert.Contains("MB/s", item.ProgressText);

        item.ApplyProgress(new DownloadProgress(100, null, 0));
        Assert.True(item.IsIndeterminate);
        Assert.Equal(0, item.Progress);
    }

    [Fact]
    public async Task Auto_closes_when_all_items_become_ready()
    {
        var vm = CreateVm();
        var finished = false;
        vm.OnFinished = value => finished = value;
        vm.AddItem("a", "A", "1 MB", false, (_, _) => Task.CompletedTask);

        await vm.InstallCoreForTestingAsync(vm.Items[0]);

        Assert.True(finished);
    }

    private static async Task WaitUntilAsync(Func<bool> condition, int timeoutMs = 5000)
    {
        var deadline = DateTime.UtcNow + TimeSpan.FromMilliseconds(timeoutMs);
        while (!condition())
        {
            if (DateTime.UtcNow > deadline)
            {
                throw new TimeoutException("等待条件超时。");
            }

            await Task.Delay(20);
        }
    }
}
