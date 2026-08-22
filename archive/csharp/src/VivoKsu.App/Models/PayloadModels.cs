namespace VivoKsu.App.Models;

public sealed record PayloadPartitionEntry(
    string Name,
    long SizeBytes,
    string CompressionType)
{
    public string SizeText => SizeBytes switch
    {
        < 1024 => $"{SizeBytes} B",
        < 1024 * 1024 => $"{SizeBytes / 1024d:F1} KB",
        < 1024L * 1024 * 1024 => $"{SizeBytes / 1024d / 1024:F1} MB",
        _ => $"{SizeBytes / 1024d / 1024 / 1024:F2} GB"
    };
}

public sealed record PayloadExtractionResult(
    string PartitionName,
    string OutputPath,
    long SizeBytes);
