using System.Windows;
using System.Windows.Input;
using System.Windows.Media;
using System.Windows.Threading;
using VivoKsu.App.Services;

namespace VivoKsu.App;

/// <summary>
/// 驱动安装提醒:启动时检测到未安装 vivo USB 驱动即弹出。
/// 点「安装驱动」以管理员身份静默运行随附安装包;点「取消」跳过(本次启动不再提醒)。
/// </summary>
public partial class DriverReminderWindow : Window
{
    private readonly VivoDriverInstaller installer;
    private readonly string? bundlePath;
    private bool installInProgress;
    private DispatcherTimer? closeTimer;

    public DriverReminderWindow(VivoDriverInstaller? installer = null)
    {
        InitializeComponent();
        this.installer = installer ?? new VivoDriverInstaller();
        bundlePath = VivoDriverInstaller.LocateBundle(AppContext.BaseDirectory);

        InstallButton.Click += async (_, _) => await InstallAsync();
        CancelButton.Click += (_, _) => RequestClose();
        CloseButton.Click += (_, _) => RequestClose();
        KeyDown += (_, e) =>
        {
            if (e.Key == Key.Escape)
            {
                RequestClose();
            }
        };
        MouseLeftButtonDown += (_, e) =>
        {
            if (e.LeftButton == MouseButtonState.Pressed)
            {
                DragMove();
            }
        };
        // 窗口被关闭(含安装中强关)时停止自动关闭定时器,避免对已关闭窗口设 DialogResult 抛异常。
        Closed += (_, _) =>
        {
            closeTimer?.Stop();
            closeTimer = null;
        };
    }

    /// <summary>请求关闭窗口。安装进行中禁止(用户以为取消了,实际 pnputil 仍在后台装)。</summary>
    private void RequestClose()
    {
        if (installInProgress)
        {
            return;
        }

        DialogResult = false;
    }

    private async Task InstallAsync()
    {
        if (bundlePath is null)
        {
            ShowStatus("未找到随附的驱动包,请从官网下载 vivo USB 驱动。", isError: true);
            return;
        }

        installInProgress = true;
        InstallButton.IsEnabled = false;
        CancelButton.IsEnabled = false;
        CloseButton.IsEnabled = false;
        BusyOverlay.Visibility = Visibility.Visible;
        try
        {
            var exitCode = await installer.InstallAsync(bundlePath);

            // 安装器返回后再做一次真实检测(驱动落地可能需要几秒)。
            var installed = await Task.Run(() => VivoDriverDetector.CreateDefault().IsInstalled());
            if (installed)
            {
                ShowStatus("驱动安装完成,可以正常连接手机。", isError: false);
                CloseAfterDelay();
                return;
            }

            ShowStatus(
                exitCode == 0
                    ? "安装已完成,但驱动尚未完全生效。请重新插拔手机,或重启 VivoKsu 后再试。"
                    : $"驱动安装失败(退出码 {exitCode})。请尝试手动以管理员身份运行,或联系管理员。",
                isError: true);
        }
        catch (OperationCanceledException)
        {
            ShowStatus("已取消管理员授权,驱动未安装。", isError: true);
        }
        catch (Exception)
        {
            ShowStatus("驱动安装失败。请手动运行安装包,或联系管理员。", isError: true);
        }
        finally
        {
            installInProgress = false;
            BusyOverlay.Visibility = Visibility.Collapsed;
            InstallButton.IsEnabled = true;
            CancelButton.IsEnabled = true;
            CloseButton.IsEnabled = true;
        }
    }

    private void ShowStatus(string message, bool isError)
    {
        StatusText.Foreground = isError
            ? (Brush)FindResource("Danger")
            : (Brush)FindResource("TealDark");
        StatusText.Text = message;
        StatusText.Visibility = Visibility.Visible;
    }

    /// <summary>安装成功后短暂展示状态,再自动关闭。</summary>
    private void CloseAfterDelay()
    {
        var timer = new DispatcherTimer { Interval = TimeSpan.FromSeconds(1.5) };
        timer.Tick += (_, _) =>
        {
            timer.Stop();
            closeTimer = null;
            if (IsVisible)
            {
                DialogResult = true;
            }
        };
        closeTimer = timer;
        timer.Start();
    }
}
