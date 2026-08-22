using System.Windows;
using System.Windows.Input;
using System.Windows.Media;
using System.Windows.Threading;
using VivoKsu.App.Services;

namespace VivoKsu.App;

/// <summary>
/// 驱动安装提醒:启动时检测到 ADB / Fastboot / MediaTek 三类驱动任缺一类即弹出;
/// 也可由「软件」页「重新安装」按钮以 reinstallMode 打开(已装也允许重装)。
/// 点「安装驱动」以管理员身份静默运行随附安装包;点「取消」跳过(本次启动不再提醒)。
/// </summary>
public partial class DriverReminderWindow : Window
{
    private readonly VivoDriverInstaller installer;
    private readonly string? bundlePath;
    private bool installInProgress;
    private DispatcherTimer? closeTimer;

    public DriverReminderWindow(VivoDriverInstaller? installer = null, bool reinstallMode = false)
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

        _ = LoadSummaryAsync(reinstallMode);
    }

    /// <summary>按三类驱动当前状态 + 是否重装模式刷新标题与说明。检测在线程池执行(DriverStore 枚举可能很慢)。</summary>
    private async Task LoadSummaryAsync(bool reinstallMode)
    {
        if (reinstallMode)
        {
            // 重装模式文案固定,无需检测(已装也允许重装)。
            TitleText.Text = "USB 驱动安装";
            DescText.Text = "可以重新安装 ADB / Fastboot / MediaTek 三类驱动。安装需要管理员权限。";
            return;
        }

        var (adb, fastboot, mediaTek) = await Task.Run(() =>
        {
            var detector = VivoDriverDetector.CreateDefault();
            return (Adb: detector.IsAdbInstalled, Fastboot: detector.IsFastbootInstalled, Mtk: detector.IsMediaTekInstalled);
        }).ConfigureAwait(true); // 回到 UI 线程更新 TextBlock

        var missing = new List<string>();
        if (!adb) missing.Add("ADB");
        if (!fastboot) missing.Add("Fastboot");
        if (!mediaTek) missing.Add("MediaTek 联发科");
        if (missing.Count == 0)
        {
            TitleText.Text = "USB 驱动已就绪";
            DescText.Text = "ADB / Fastboot / MediaTek 三类驱动均已安装,可以正常连接手机。";
        }
        else
        {
            TitleText.Text = "缺少手机 USB 驱动";
            DescText.Text = $"当前电脑缺少 {string.Join("、", missing)} 驱动,刷机 / 救砖 / 文件管理等功能可能无法识别手机。安装需要管理员权限。";
        }
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

            // 安装器返回后再做一次三类真实检测(驱动落地可能需要几秒)。
            var status = await Task.Run(() =>
            {
                var detector = VivoDriverDetector.CreateDefault();
                return (Adb: detector.IsAdbInstalled, Fastboot: detector.IsFastbootInstalled, Mtk: detector.IsMediaTekInstalled);
            });
            if (status.Adb && status.Fastboot && status.Mtk)
            {
                ShowStatus("驱动安装完成,可以正常连接手机。", isError: false);
                CloseAfterDelay();
                return;
            }

            var stillMissing = new List<string>();
            if (!status.Adb) stillMissing.Add("ADB");
            if (!status.Fastboot) stillMissing.Add("Fastboot");
            if (!status.Mtk) stillMissing.Add("MediaTek");
            ShowStatus(
                exitCode == 0
                    ? $"安装已完成,但 {string.Join("、", stillMissing)} 尚未被系统登记。可尝试以管理员身份手动安装,或重新下载驱动包后重试。"
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
