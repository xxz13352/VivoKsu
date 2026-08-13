namespace VivoKsu.App.Models;

public sealed record DeviceFileEntry(string Name, string FullPath, bool IsDirectory, long SizeBytes);
