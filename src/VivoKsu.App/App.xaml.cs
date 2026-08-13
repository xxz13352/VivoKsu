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
            WriteCrashLog(e.Exception);
            e.Handled = true;
            MessageBox.Show("发生错误: " + e.Exception.Message, "VivoKsu", MessageBoxButton.OK, MessageBoxImage.Error);
        };
        AppDomain.CurrentDomain.UnhandledException += (_, e) =>
            WriteCrashLog(e.ExceptionObject as Exception);

        // 登录门禁(商业工具):账号+密码验证通过才进入主界面。
        var preferences = ToolPathPreferences.CreateDefault();
        using var loginService = new LoginService();
        string? token;
        if (!string.IsNullOrWhiteSpace(preferences.Token))
        {
            // 记住登录:本地 token 有效则直接进入。
            var name = loginService.ValidateTokenAsync(preferences.Token, CancellationToken.None).GetAwaiter().GetResult();
            token = name is not null ? preferences.Token : null;
        }
        else
        {
            token = null;
        }

        if (token is null)
        {
            var login = new LoginWindow(preferences, loginService);
            if (login.ShowDialog() != true)
            {
                Shutdown();
                return;
            }

            token = login.Token;
        }

        composition = AppComposition.CreateDefault();
        composition.SetAuthToken(token!);
        var mainWindow = new MainWindow(composition);
        mainWindow.Closed += (_, _) => Shutdown();
        MainWindow = mainWindow;
        mainWindow.Show();
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
