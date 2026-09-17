using System.Collections.ObjectModel;
using System.IO;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using Microsoft.Win32;
using VivoKsu.App.Models;
using VivoKsu.App.Services;

namespace VivoKsu.App.ViewModels;

public partial class FileManagerViewModel : ObservableObject
{
    private readonly DeviceSessionViewModel session;
    private readonly AdbFileService files;
    private readonly OperationLogService logs;
    private readonly IOperationCoordinator? coordinator;
    private readonly Func<string, string, string?>? saveLocationPicker;
    private readonly Action<string>? notifyError;

    [ObservableProperty]
    [NotifyCanExecuteChangedFor(nameof(GoUpLocalCommand))]
    private string currentLocalPath = Environment.GetFolderPath(Environment.SpecialFolder.DesktopDirectory);

    [ObservableProperty]
    [NotifyCanExecuteChangedFor(nameof(GoUpRemoteCommand))]
    private string currentRemotePath = "/sdcard";

    [ObservableProperty]
    [NotifyCanExecuteChangedFor(nameof(UploadCommand))]
    [NotifyCanExecuteChangedFor(nameof(NavigateLocalCommand))]
    private DeviceFileEntry? selectedLocal;

    [ObservableProperty]
    [NotifyCanExecuteChangedFor(nameof(DownloadCommand))]
    [NotifyCanExecuteChangedFor(nameof(RequestDeleteCommand))]
    [NotifyCanExecuteChangedFor(nameof(NavigateRemoteCommand))]
    private DeviceFileEntry? selectedRemote;

    [ObservableProperty]
    private bool isDeleteConfirmationVisible;

    [ObservableProperty]
    [NotifyCanExecuteChangedFor(nameof(ConfirmDeleteCommand))]
    private DeviceFileEntry? pendingDeleteEntry;

    public FileManagerViewModel(
        DeviceSessionViewModel session,
        AdbFileService files,
        OperationLogService logs,
        IOperationCoordinator? coordinator = null,
        Func<string, string, string?>? saveLocationPicker = null,
        Action<string>? notifyError = null)
    {
        this.session = session;
        this.files = files;
        this.logs = logs;
        this.coordinator = coordinator;
        this.saveLocationPicker = saveLocationPicker;
        this.notifyError = notifyError;
        RefreshCommand = new AsyncRelayCommand(RefreshAsync, CanUseAdb);
        NavigateLocalCommand = new RelayCommand<DeviceFileEntry?>(NavigateLocal, entry => entry is { IsDirectory: true });
        NavigateRemoteCommand = new AsyncRelayCommand<DeviceFileEntry?>(NavigateRemoteAsync, entry => entry is { IsDirectory: true } && CanUseAdb());
        GoUpLocalCommand = new RelayCommand(GoUpLocal, CanGoUpLocal);
        GoUpRemoteCommand = new AsyncRelayCommand(GoUpRemoteAsync, CanGoUpRemote);
        UploadCommand = new AsyncRelayCommand(UploadAsync, CanUpload);
        DownloadCommand = new AsyncRelayCommand(DownloadAsync, () => SelectedRemote is { IsDirectory: false } && CanUseAdb());
        RequestDeleteCommand = new RelayCommand(RequestDelete, () => SelectedRemote is not null && CanUseAdb());
        ConfirmDeleteCommand = new AsyncRelayCommand(ConfirmDeleteAsync, () => PendingDeleteEntry is not null && CanUseAdb());
        CancelDeleteCommand = new RelayCommand(CancelDelete);
        InstallApkCommand = new AsyncRelayCommand(InstallApkAsync, CanUseAdb);
        session.PropertyChanged += OnSessionPropertyChanged;

        RefreshLocal();
    }

    public ObservableCollection<DeviceFileEntry> LocalFiles { get; } = [];
    public ObservableCollection<DeviceFileEntry> RemoteFiles { get; } = [];
    public IAsyncRelayCommand RefreshCommand { get; }
    public IRelayCommand<DeviceFileEntry?> NavigateLocalCommand { get; }
    public IAsyncRelayCommand<DeviceFileEntry?> NavigateRemoteCommand { get; }
    public IRelayCommand GoUpLocalCommand { get; }
    public IAsyncRelayCommand GoUpRemoteCommand { get; }
    public IAsyncRelayCommand UploadCommand { get; }
    public IAsyncRelayCommand DownloadCommand { get; }
    public IRelayCommand RequestDeleteCommand { get; }
    public IAsyncRelayCommand ConfirmDeleteCommand { get; }
    public IRelayCommand CancelDeleteCommand { get; }
    public IAsyncRelayCommand InstallApkCommand { get; }

    private async Task RefreshAsync()
    {
        RefreshLocal();
        await RefreshRemoteAsync();
    }

    public void NavigateLocal(DeviceFileEntry? entry)
    {
        if (entry is not { IsDirectory: true } || !Directory.Exists(entry.FullPath))
        {
            return;
        }

        if (!TryReadLocalEntries(entry.FullPath, out var entries))
        {
            return;
        }

        CurrentLocalPath = entry.FullPath;
        SelectedLocal = null;
        ReplaceLocalEntries(entries);
    }

    public async Task NavigateRemoteAsync(DeviceFileEntry? entry)
    {
        if (entry is not { IsDirectory: true })
        {
            return;
        }

        var previousPath = CurrentRemotePath;
        CurrentRemotePath = entry.FullPath;
        SelectedRemote = null;
        if (!await RefreshRemoteAsync())
        {
            CurrentRemotePath = previousPath;
            SelectedRemote = entry;
        }
    }

    private void GoUpLocal()
    {
        var parent = Directory.GetParent(CurrentLocalPath);
        if (parent is null)
        {
            return;
        }

        if (!TryReadLocalEntries(parent.FullName, out var entries))
        {
            return;
        }

        CurrentLocalPath = parent.FullName;
        SelectedLocal = null;
        ReplaceLocalEntries(entries);
    }

    private async Task GoUpRemoteAsync()
    {
        var previousPath = CurrentRemotePath;
        var previousSelection = SelectedRemote;
        CurrentRemotePath = GetRemoteParent(CurrentRemotePath);
        SelectedRemote = null;
        if (!await RefreshRemoteAsync())
        {
            CurrentRemotePath = previousPath;
            SelectedRemote = previousSelection;
        }
    }

    private bool CanGoUpLocal() => Directory.GetParent(CurrentLocalPath) is not null;

    private bool CanUpload() => CanUseAdb();

    private void OnSessionPropertyChanged(object? sender, System.ComponentModel.PropertyChangedEventArgs args)
    {
        if (args.PropertyName == nameof(DeviceSessionViewModel.Serial)
            || (args.PropertyName == nameof(DeviceSessionViewModel.ConnectionState) && !session.IsAdbConnected))
        {
            ResetRemoteState();
        }

        if (args.PropertyName is nameof(DeviceSessionViewModel.ConnectionState) or nameof(DeviceSessionViewModel.IsBusy))
        {
            RefreshCommand.NotifyCanExecuteChanged();
            NavigateRemoteCommand.NotifyCanExecuteChanged();
            GoUpRemoteCommand.NotifyCanExecuteChanged();
            UploadCommand.NotifyCanExecuteChanged();
            DownloadCommand.NotifyCanExecuteChanged();
            RequestDeleteCommand.NotifyCanExecuteChanged();
            ConfirmDeleteCommand.NotifyCanExecuteChanged();
            InstallApkCommand.NotifyCanExecuteChanged();
        }
    }

    private void ResetRemoteState()
    {
        CancelDelete();
        SelectedRemote = null;
        RemoteFiles.Clear();
        CurrentRemotePath = "/sdcard";
    }

    private bool CanUseAdb() => session.IsAdbConnected && !session.IsBusy;

    private bool CanGoUpRemote() => CanUseAdb() && NormalizeRemotePath(CurrentRemotePath) != "/";

    private async Task<bool> RefreshRemoteAsync()
    {
        if (session.ConnectionState != DeviceConnectionState.AdbConnected)
        {
            logs.Write(OperationLogLevel.Warning, "ADB 设备未就绪，无法读取设备文件。 ");
            return false;
        }

        if (coordinator is not null)
        {
            return await RunCoordinatedAsync(
                OperationKind.Discovering,
                "正在读取设备目录",
                (context, cancellationToken) => RefreshRemoteCoreAsync(cancellationToken, context));
        }

        try
        {
            await RefreshRemoteCoreAsync(CancellationToken.None);
            return true;
        }
        catch (Exception exception)
        {
            logs.Write(OperationLogLevel.Error, exception.Message);
            return false;
        }
    }

    private async Task RefreshRemoteCoreAsync(CancellationToken cancellationToken, OperationContext? context = null)
    {
        var entries = await files.ListRemoteAsync(session.Serial, CurrentRemotePath, cancellationToken, context);
        RemoteFiles.Clear();
        foreach (var entry in entries)
        {
            RemoteFiles.Add(entry);
        }

        if (context is null)
        {
            logs.Write(OperationLogLevel.Success, "设备目录已更新。");
        }
        else
        {
            context.ReportStage("设备目录已更新");
        }
    }

    private async Task UploadAsync()
    {
        if (session.ConnectionState != DeviceConnectionState.AdbConnected)
        {
            return;
        }

        var localPath = SelectedLocal?.IsDirectory == false ? SelectedLocal.FullPath : null;
        if (localPath is null)
        {
            var dialog = new OpenFileDialog
            {
                Filter = "所有文件 (*.*)|*.*|Android 安装包 (*.apk)|*.apk|镜像文件 (*.img;*.bin)|*.img;*.bin",
                CheckFileExists = true,
                Multiselect = false,
                Title = "选择要传入手机的文件"
            };

            if (dialog.ShowDialog() != true)
            {
                return;
            }

            localPath = dialog.FileName;
        }

        var fileName = Path.GetFileName(localPath);
        if (coordinator is not null)
        {
            await RunCoordinatedAsync(OperationKind.Transferring, $"正在上传 {fileName}", async (context, cancellationToken) =>
            {
                await files.UploadAsync(session.Serial, localPath, CurrentRemotePath, cancellationToken, context);
                await RefreshRemoteAfterMutationAsync("文件上传", cancellationToken, context);
            });
            return;
        }

        session.BeginOperation(OperationKind.Transferring, $"正在上传 {fileName}");
        try
        {
            await files.UploadAsync(session.Serial, localPath, CurrentRemotePath, CancellationToken.None);
            session.CompleteOperation("文件上传完成");
            await RefreshAsync();
        }
        catch (Exception exception)
        {
            session.FailOperation("文件上传失败");
            logs.Write(OperationLogLevel.Error, exception.Message);
        }
    }

    private async Task DownloadAsync()
    {
        if (SelectedRemote is null || session.ConnectionState != DeviceConnectionState.AdbConnected)
        {
            return;
        }

        var selected = SelectedRemote;
        var destination = PickSaveLocation(selected.Name);
        if (destination is null)
        {
            return; // 用户取消保存对话框,不下载。
        }

        if (coordinator is not null)
        {
            var succeeded = await RunCoordinatedAsync(OperationKind.Transferring, $"正在下载 {selected.Name}", async (context, cancellationToken) =>
            {
                await files.DownloadToFileAsync(session.Serial, selected, destination, cancellationToken, context);
                FollowDownloadedLocation(destination);
            });
            // RunCoordinatedAsync 吞掉异常;经会话 Failed 态识别失败后对用户可见(生产=MessageBox)。
            if (!succeeded && session.OperationKind == OperationKind.Failed)
            {
                NotifyDownloadFailure($"下载 {selected.Name} 失败,请查看操作日志了解原因。");
            }

            return;
        }

        session.BeginOperation(OperationKind.Transferring, $"正在下载 {selected.Name}");
        try
        {
            await files.DownloadToFileAsync(session.Serial, selected, destination, CancellationToken.None);
            session.CompleteOperation("文件下载完成");
            FollowDownloadedLocation(destination);
        }
        catch (Exception exception)
        {
            session.FailOperation("文件下载失败");
            logs.Write(OperationLogLevel.Error, exception.Message);
            NotifyDownloadFailure($"下载 {selected.Name} 失败:{exception.Message}");
        }
    }

    /// <summary>下载失败对用户可见(生产=MessageBox;单测注入回调断言)。</summary>
    private void NotifyDownloadFailure(string message)
    {
        if (notifyError is not null)
        {
            notifyError(message);
        }
        else
        {
            logs.Write(OperationLogLevel.Error, message);
        }
    }

    private string? PickSaveLocation(string defaultFileName)
    {
        if (saveLocationPicker is not null)
        {
            return saveLocationPicker(CurrentLocalPath, defaultFileName);
        }

        var dialog = new SaveFileDialog
        {
            FileName = defaultFileName,
            InitialDirectory = CurrentLocalPath,
            Filter = "所有文件 (*.*)|*.*",
            Title = "选择保存位置"
        };
        return dialog.ShowDialog() == true ? dialog.FileName : null;
    }

    private void FollowDownloadedLocation(string destinationFilePath)
    {
        var directory = Path.GetDirectoryName(destinationFilePath);
        if (string.IsNullOrWhiteSpace(directory))
        {
            return;
        }

        CurrentLocalPath = directory;
        RefreshLocal();
    }

    private void RequestDelete()
    {
        if (SelectedRemote is null)
        {
            return;
        }

        PendingDeleteEntry = SelectedRemote;
        IsDeleteConfirmationVisible = true;
    }

    private void CancelDelete()
    {
        IsDeleteConfirmationVisible = false;
        PendingDeleteEntry = null;
    }

    private async Task ConfirmDeleteAsync()
    {
        var pendingEntry = PendingDeleteEntry;
        if (pendingEntry is null || session.ConnectionState != DeviceConnectionState.AdbConnected)
        {
            return;
        }

        IsDeleteConfirmationVisible = false;
        PendingDeleteEntry = null;
        if (coordinator is not null)
        {
            await RunCoordinatedAsync(OperationKind.Transferring, $"正在删除 {pendingEntry.Name}", async (context, cancellationToken) =>
            {
                await files.DeleteRemoteAsync(session.Serial, pendingEntry, cancellationToken, context);
                if (SelectedRemote == pendingEntry)
                {
                    SelectedRemote = null;
                }

                await RefreshRemoteAfterMutationAsync("文件删除", cancellationToken, context);
            });
            return;
        }

        try
        {
            await files.DeleteRemoteAsync(session.Serial, pendingEntry, CancellationToken.None);
            if (SelectedRemote == pendingEntry)
            {
                SelectedRemote = null;
            }

            await RefreshAsync();
        }
        catch (Exception exception)
        {
            logs.Write(OperationLogLevel.Error, exception.Message);
        }
    }

    private async Task InstallApkAsync()
    {
        var dialog = new OpenFileDialog { Filter = "Android package (*.apk)|*.apk", CheckFileExists = true, Multiselect = false };
        if (dialog.ShowDialog() != true || session.ConnectionState != DeviceConnectionState.AdbConnected)
        {
            return;
        }

        if (coordinator is not null)
        {
            await RunCoordinatedAsync(OperationKind.Installing, $"正在安装 {Path.GetFileName(dialog.FileName)}", async (context, cancellationToken) =>
            {
                await files.InstallApkAsync(session.Serial, dialog.FileName, cancellationToken, context);
            });
            return;
        }

        session.BeginOperation(OperationKind.Installing, "正在安装 APK");
        try
        {
            await files.InstallApkAsync(session.Serial, dialog.FileName, CancellationToken.None);
            session.CompleteOperation("APK 安装完成");
        }
        catch (Exception exception)
        {
            session.FailOperation("APK 安装失败");
            logs.Write(OperationLogLevel.Error, exception.Message);
        }
    }

    private void RefreshLocal()
    {
        if (TryReadLocalEntries(CurrentLocalPath, out var entries))
        {
            ReplaceLocalEntries(entries);
        }
    }

    private bool TryReadLocalEntries(string path, out List<DeviceFileEntry> entries)
    {
        entries = [];
        try
        {
            entries.AddRange(Directory.EnumerateDirectories(path)
                .OrderBy(directory => directory, StringComparer.OrdinalIgnoreCase)
                .Select(directory => new DeviceFileEntry(Path.GetFileName(directory), directory, true, 0)));
            entries.AddRange(Directory.EnumerateFiles(path)
                .OrderBy(file => file, StringComparer.OrdinalIgnoreCase)
                .Select(file => new DeviceFileEntry(Path.GetFileName(file), file, false, new FileInfo(file).Length)));
            return true;
        }
        catch (Exception exception)
        {
            logs.Write(OperationLogLevel.Error, $"读取本地目录失败: {exception.Message}");
            entries = [];
            return false;
        }
    }

    private void ReplaceLocalEntries(IEnumerable<DeviceFileEntry> entries)
    {
        LocalFiles.Clear();
        foreach (var entry in entries)
        {
            LocalFiles.Add(entry);
        }
    }

    private async Task RefreshRemoteAfterMutationAsync(
        string operationName,
        CancellationToken cancellationToken,
        OperationContext context)
    {
        try
        {
            await RefreshRemoteCoreAsync(cancellationToken, context);
        }
        catch (OperationCanceledException)
        {
            throw;
        }
        catch (Exception exception)
        {
            logs.Write(OperationLogLevel.Warning, $"{operationName}已完成，但设备目录刷新失败: {exception.Message}");
            context.ReportStage($"{operationName}完成，目录刷新失败");
        }
    }

    private async Task<bool> RunCoordinatedAsync(
        OperationKind kind,
        string title,
        Func<OperationContext, CancellationToken, Task> operation)
    {
        try
        {
            await coordinator!.RunAsync(kind, title, operation);
            return true;
        }
        catch (OperationCanceledException)
        {
            return false;
        }
        catch (Exception)
        {
            return false;
        }
    }

    private static string GetRemoteParent(string path)
    {
        var normalized = NormalizeRemotePath(path);
        if (normalized == "/")
        {
            return "/";
        }

        var separator = normalized.LastIndexOf('/');
        return separator <= 0 ? "/" : normalized[..separator];
    }

    private static string NormalizeRemotePath(string path)
    {
        if (string.IsNullOrWhiteSpace(path))
        {
            return "/";
        }

        var normalized = path.Trim();
        return normalized == "/" ? "/" : normalized.TrimEnd('/');
    }
}
