namespace VivoKsu.App.Services;

/// <summary>
/// fastboot CLI 执行器抽象(唯一 fastboot.exe)。
/// 服务依赖此接口便于测试注入 fake;生产实现为 <see cref="FastbootCliRunner"/>。
/// </summary>
public interface IFastbootCliRunner
{
    bool IsAvailable { get; }

    /// <summary><c>fastboot flash</c>,带连续传输进度(0-1)。失败抛 <see cref="FastbootCliException"/>。</summary>
    Task FlashAsync(string serial, string partition, string imagePath, IProgress<double>? progress, CancellationToken cancellationToken);

    /// <summary><c>fastboot erase</c>。</summary>
    Task EraseAsync(string serial, string partition, CancellationToken cancellationToken);

    /// <summary><c>fastboot getvar</c>,返回剥离 bootloader 前缀后的值;all 返回原始多行输出。</summary>
    Task<string> GetVarAsync(string serial, string variable, CancellationToken cancellationToken);

    /// <summary>探测分区是否存在(getvar partition-type),区分「无分区」与「传输失败」。</summary>
    Task<bool> PartitionExistsAsync(string serial, string partition, CancellationToken cancellationToken);

    /// <summary><c>fastboot reboot</c>。</summary>
    Task RebootAsync(string serial, CancellationToken cancellationToken);

    /// <summary><c>fastboot set_active</c>。</summary>
    Task SetActiveAsync(string serial, string slot, CancellationToken cancellationToken);
}
