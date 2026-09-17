using System.IO;
using System.Windows;
using System.Windows.Threading;
using VivoKsu.App.Models;
using VivoKsu.App.Services;

namespace VivoKsu.App;

public partial class App : Application
{
    private AppComposition? composition;
    private static readonly CrashReporter CrashLog = new();

    /// <summary>记录未捕获异常(格式对齐 Rust:[epoch] panic: ...,供下次启动补传 /api/diagnostics/crash)。</summary>
    private static void WriteCrashLog(Exception? exception) => CrashLog.Write(exception);

    /// <summary>
    /// 启动后台服务端任务(对齐 Rust 启动流程):先刷新签名 pinset(补充钉扎名单,失败沿用
    /// 内置双 pin + 已有缓存),再补传上次崩溃(延迟 8s 避开启动关键路径;匿名可报,成功后
    /// 清空 crash.log,失败保留下次重试)。任何异常都不影响启动。
    /// </summary>
    private static async Task RunStartupServerTasksAsync()
    {
        try
        {
            await Task.Delay(CrashReporter.StartupDelay).ConfigureAwait(true);
            var client = new OtaApiClient();
            try
            {
                await client.RefreshPinsetAsync(CancellationToken.None).ConfigureAwait(true);
            }
            catch
            {
                // 刷新失败沿用内置双 pin + 已有缓存;离线/服务端故障都不影响启动。
            }

            await new CrashReporter()
                .UploadPendingAsync(client, ClientSession.ProcessNonce, CancellationToken.None)
                .ConfigureAwait(true);
        }
        catch
        {
            // 补传失败静默:crash.log 保留,下次启动重试。
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
            MessageBox.Show("发生错误: " + e.Exception.Message, "奶蛙Flash", MessageBoxButton.OK, MessageBoxImage.Error);
        };
        AppDomain.CurrentDomain.UnhandledException += (_, e) =>
            WriteCrashLog(e.ExceptionObject as Exception);

        // TLS 钉扎失败 → 完整性遥测(pin_validation/pin_mismatch):IntegrityReporter 自带 10s 限频。
        ApiTlsPinPolicy.PinRejected += _ => new IntegrityReporter(new OtaApiClient())
            .Report(IntegrityReportPhase.PinValidation, IntegrityReportReason.PinMismatch);

        // 后台启动任务:pinset 刷新 + 上次崩溃补传(不阻塞启动)。
        _ = RunStartupServerTasksAsync();

        // 版本门禁:打开软件即校验;版本低于后台「版本号控制」最低版本 → 强制更新,不进登录。
        if (BlockForForcedUpdate())
        {
            Shutdown();
            return;
        }

        // 登出后要回到登录窗而不退出程序:关窗不再自动退出,由代码显式 Shutdown。
        ShutdownMode = ShutdownMode.OnExplicitShutdown;
        RunApplicationLoop();
    }

    private bool isLogout;

    /// <summary>登录循环:登录成功 → 新 composition + 主窗;登出 → 关闭主窗后重入本循环;退出 → Shutdown。</summary>
    private void RunApplicationLoop()
    {
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
            composition.LogoutRequested += OnLogoutRequested;
            // 注入 token + 启动在线会话(心跳 / 强制下线监听 / 在线状态轮询)。
            // 会话 id 来自登录(服务端租约已按其创建),心跳必须复用同一 id。
            composition.StartSessionAsync(token!, login.Username ?? string.Empty, login.SessionId!);
            var mainWindow = new MainWindow(composition);
            mainWindow.Closed += OnMainWindowClosed;
            MainWindow = mainWindow;
            mainWindow.Show();

            // 驱动提醒:后台检测手机 USB 驱动,未安装则弹「安装/取消」窗(不阻塞主界面)。
            // 先做「组件安装」检测(缺失资源才弹模态窗,可跳过),避免与驱动提醒双模态叠放。
            ShowResourceDownloaderIfNeeded();
            CheckAndRemindDriverAsync();
        }
        catch (UpdateRequiredException update)
        {
            // 登录请求返回 426(绕过启动校验的兜底路径):强制更新。
            ShowUpdateRequired(update.Latest, update.MinVersion, update.DownloadUrl);
            Shutdown();
        }
    }

    private void OnLogoutRequested(object? sender, EventArgs eventArgs)
    {
        isLogout = true;
        MainWindow?.Close();
    }

    private void OnMainWindowClosed(object? sender, EventArgs eventArgs)
    {
        if (isLogout)
        {
            isLogout = false;
            RunApplicationLoop();
        }
        else
        {
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
    /// 组件安装检测:登录后扫描外置资源(scrcpy / APK / payload_dumper)是否就绪,
    /// 有缺失才弹「组件安装」模态窗(可选装/全装/跳过);全就绪静默跳过。
    /// 检测/弹窗失败不打扰客户(资源仍是首次使用时按需下载)。
    /// </summary>
    private void ShowResourceDownloaderIfNeeded()
    {
        try
        {
            if (composition is null)
            {
                return;
            }

            var viewModel = composition.CreateResourceDownloadViewModel();
            viewModel.Detect();
            if (viewModel.HasMissing)
            {
                new ResourceDownloadWindow(viewModel).ShowDialog();
                // 关窗后刷新软件页组件状态:下载器可能刚把 scrcpy/APK/payload 装进 C:\nwflash。
                composition.MainViewModel.Software.RefreshCommand.Execute(null);
            }
        }
        catch
        {
            // 忽略:资源缺失不影响登录后使用,首次操作时仍会自动下载。
        }
    }

    /// <summary>
    /// 驱动检测提醒:后台扫描驱动安装信号,未安装则弹「安装/取消」窗。
    /// 检测或弹窗失败都不阻塞主界面使用。
    /// </summary>
    private async void CheckAndRemindDriverAsync()
    {
        try
        {
            // 只要求 ADB + Fastboot(刷机/连接必需);MediaTek 联发科仅救砖用,高通 SoC 机型(iQOO/X 系)
            // 不需要,不强制提醒——「软件」页三类分别显示状态,可手动重装。
            var needed = await Task.Run(() =>
            {
                var detector = VivoDriverDetector.CreateDefault();
                return (Adb: detector.IsAdbInstalled, Fastboot: detector.IsFastbootInstalled);
            });
            if (needed.Adb && needed.Fastboot)
            {
                return;
            }

            // 模态提醒:需用户决定是否安装后主界面才可操作(UAC 安装需聚焦)。
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
