using System.IO;
using System.Windows;
using System.Windows.Threading;
using VivoKsu.App.Services;

namespace VivoKsu.App;

public partial class App : Application
{
    private AppComposition? composition;

    private static void WriteCrashLog(Exception? exception)
    {
        if (exception is null)
        {
            return;
        }

        try
        {
            var directory = Path.Combine(
                Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
                "VivoKsu");
            Directory.CreateDirectory(directory);
            File.AppendAllText(
                Path.Combine(directory, "crash.log"),
                $"[{DateTime.Now:yyyy-MM-dd HH:mm:ss}] {exception}{Environment.NewLine}{Environment.NewLine}");
        }
        catch
        {
            // 日志写失败忽略。
        }
    }

    protected override void OnStartup(StartupEventArgs eventArgs)
    {
        base.OnStartup(eventArgs);

        // 崩溃日志(商业工具排查):记录未捕获异常到本地文件。
        DispatcherUnhandledException += (_, e) =>
        {
            // 强制更新:运行期任一请求返回 426 → 弹更新窗并退出(无跳过路径)。
            if (e.Exception is UpdateRequiredException update)
            {
                WriteCrashLog(e.Exception);
                e.Handled = true;
                ShowUpdateRequired(update.Latest, update.MinVersion, update.DownloadUrl);
                Shutdown();
                return;
            }

            WriteCrashLog(e.Exception);
            e.Handled = true;
            MessageBox.Show("发生错误: " + e.Exception.Message, "Nwflash", MessageBoxButton.OK, MessageBoxImage.Error);
        };
        AppDomain.CurrentDomain.UnhandledException += (_, e) =>
            WriteCrashLog(e.ExceptionObject as Exception);

        // 版本门禁:打开软件即校验;版本低于后台「版本号控制」最低版本 → 强制更新,不进登录。
        if (BlockForForcedUpdate())
        {
            Shutdown();
            return;
        }

        // 登录门禁(商业工具):每次启动强制账号+密码登录,验证通过才进入主界面。
        try
        {
            using var loginService = new LoginService();
            var login = new LoginWindow(loginService);
            if (login.ShowDialog() != true)
            {
                Shutdown();
                return;
            }

            var token = login.Token;

            composition = AppComposition.CreateDefault();
            // 注入 token + 启动在线会话(心跳 / 强制下线监听 / 在线状态轮询)。
            composition.StartSessionAsync(token!, login.Username ?? string.Empty);
            var mainWindow = new MainWindow(composition);
            mainWindow.Closed += (_, _) => Shutdown();
            MainWindow = mainWindow;
            mainWindow.Show();

            // 驱动提醒:后台检测手机 USB 驱动,未安装则弹「安装/取消」窗(不阻塞主界面)。
            CheckAndRemindDriverAsync();
        }
        catch (UpdateRequiredException update)
        {
            // 登录请求返回 426(绕过启动校验的兜底路径):强制更新。
            ShowUpdateRequired(update.Latest, update.MinVersion, update.DownloadUrl);
            Shutdown();
        }
    }

    /// <summary>启动版本校验:低于后台最低版本 → 弹强制更新窗并返回 true(调用方应退出)。网络失败放行。</summary>
    private bool BlockForForcedUpdate()
    {
        try
        {
            using var versionService = new AppVersionService();
            var check = versionService.CheckAsync(CancellationToken.None).GetAwaiter().GetResult();
            if (check.ForceUpdate)
            {
                ShowUpdateRequired(check.Latest, check.MinVersion, check.DownloadUrl);
                return true;
            }
        }
        catch
        {
            // 校验失败(离线等)不阻塞启动;后续请求的 426 会兜底。
        }

        return false;
    }

    /// <summary>弹强制更新窗(无跳过路径,关闭即视为放弃使用,调用方随后 Shutdown)。</summary>
    private void ShowUpdateRequired(string? latest, string? minVersion, string? downloadUrl)
    {
        var window = new UpdateRequiredWindow(latest, minVersion, downloadUrl);
        window.ShowDialog();
    }

    /// <summary>
    /// 驱动检测提醒:后台扫描驱动安装信号,未安装则弹「安装/取消」窗。
    /// 检测或弹窗失败都不阻塞主界面使用。
    /// </summary>
    private async void CheckAndRemindDriverAsync()
    {
        try
        {
            var installed = await Task.Run(() => VivoDriverDetector.CreateDefault().IsInstalled());
            if (installed)
            {
                return;
            }

            new DriverReminderWindow().ShowDialog();
        }
        catch
        {
            // 驱动检测失败(权限/路径异常等)不打扰客户。
        }
    }

    protected override void OnExit(ExitEventArgs eventArgs)
    {
        if (composition is not null)
        {
            // Block shutdown until cleanup completes, pumping the dispatcher so any
            // UI-context continuation (e.g. the device-monitor loop) can still resume.
            var frame = new DispatcherFrame();
            var timeout = new DispatcherTimer { Interval = TimeSpan.FromSeconds(5) };
            timeout.Tick += (_, _) =>
            {
                timeout.Stop();
                frame.Continue = false;
            };
            timeout.Start();
            Task.Run(async () =>
            {
                try
                {
                    await composition.StopAsync();
                }
                finally
                {
                    frame.Continue = false;
                }
            });
            Dispatcher.PushFrame(frame);
        }

        base.OnExit(eventArgs);
    }
}
