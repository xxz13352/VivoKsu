using CommunityToolkit.Mvvm.ComponentModel;
using VivoKsu.App.Models;

namespace VivoKsu.App.ViewModels;

public sealed partial class PayloadPartitionItemViewModel : ObservableObject
{
    public PayloadPartitionItemViewModel(PayloadPartitionEntry entry)
    {
        Name = entry.Name;
        SizeBytes = entry.SizeBytes;
        SizeText = entry.SizeText;
        CompressionType = entry.CompressionType;
        FullPath = entry.Name;
    }

    public PayloadPartitionItemViewModel(string name, long sizeBytes, string? fullPath = null)
    {
        Name = name;
        SizeBytes = sizeBytes;
        SizeText = FormatBytes(sizeBytes);
        CompressionType = string.Empty;
        FullPath = fullPath ?? name;
    }

    public string Name { get; }

    public long SizeBytes { get; }

    public string SizeText { get; }

    public string CompressionType { get; }

    public string FullPath { get; }

    [ObservableProperty]
    private bool isSelected;

    private static string FormatBytes(long bytes) => bytes switch
    {
        < 1024 => $"{bytes} B",
        < 1024 * 1024 => $"{bytes / 1024d:F1} KB",
        < 1024L * 1024 * 1024 => $"{bytes / 1024d / 1024:F1} MB",
        _ => $"{bytes / 1024d / 1024 / 1024:F2} GB"
    };
}
