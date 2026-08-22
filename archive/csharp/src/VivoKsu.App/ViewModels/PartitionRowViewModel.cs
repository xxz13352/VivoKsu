using CommunityToolkit.Mvvm.ComponentModel;
using VivoKsu.App.Models;

namespace VivoKsu.App.ViewModels;

public partial class PartitionRowViewModel : ObservableObject
{
    public PartitionRowViewModel(DevicePartition partition)
    {
        Partition = partition;
    }

    public DevicePartition Partition { get; }

    public string Name => Partition.Name;

    public string DevicePath => Partition.DevicePath;

    public string Slot => string.IsNullOrWhiteSpace(Partition.Slot) ? "--" : Partition.Slot.ToUpperInvariant();

    public string SizeText => FormatBytes(Partition.SizeBytes);

    public bool IsMounted => Partition.IsMounted;

    public bool IsHighRisk => Partition.IsHighRisk;

    public bool CanBackup => Partition.CanBackup;

    public string RiskLabel => IsHighRisk ? "高风险" : IsMounted ? "已挂载" : "常规";

    [ObservableProperty]
    private bool isSelected;

    [ObservableProperty]
    private string? imagePath;

    [ObservableProperty]
    private PartitionTaskState taskState = PartitionTaskState.Waiting;

    [ObservableProperty]
    private double progress;

    [ObservableProperty]
    private string transferText = "--";

    public string StateText => TaskState switch
    {
        PartitionTaskState.Waiting => "等待",
        PartitionTaskState.Running => "执行中",
        PartitionTaskState.Succeeded => "完成",
        PartitionTaskState.Failed => "失败",
        PartitionTaskState.Canceled => "已停止",
        _ => "--"
    };

    public bool HasImage => !string.IsNullOrWhiteSpace(ImagePath);

    partial void OnImagePathChanged(string? value) => OnPropertyChanged(nameof(HasImage));

    partial void OnTaskStateChanged(PartitionTaskState value) => OnPropertyChanged(nameof(StateText));

    public void ResetTransferState()
    {
        TaskState = PartitionTaskState.Waiting;
        Progress = 0;
        TransferText = "--";
    }

    public void ApplyProgress(PartitionTransferProgress value)
    {
        var total = value.TotalBytes;
        Progress = total is > 0 ? Math.Clamp(value.TransferredBytes / (double)total.Value, 0, 1) : 0;
        TransferText = total is > 0
            ? $"{FormatBytes(value.TransferredBytes)} / {FormatBytes(total.Value)}"
            : FormatBytes(value.TransferredBytes);
    }

    private static string FormatBytes(long? bytes) => bytes switch
    {
        null or <= 0 => "--",
        < 1024 => $"{bytes} B",
        < 1024 * 1024 => $"{bytes / 1024d:F1} KB",
        < 1024L * 1024 * 1024 => $"{bytes / 1024d / 1024:F1} MB",
        _ => $"{bytes / 1024d / 1024 / 1024:F2} GB"
    };
}
