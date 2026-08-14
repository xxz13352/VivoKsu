using System.Diagnostics;
using System.Windows;
using VivoKsu.App.Services;

namespace VivoKsu.App;

/// <summary>强制更新拦截窗:无跳过路径 —— 打开下载链接(若有)或退出。</summary>
public partial class UpdateRequiredWindow : Window
{
    public UpdateRequiredWindow(string? latest, string? minVersion, string? downloadUrl)
    {
        InitializeComponent();
        LatestText.Text = string.IsNullOrWhiteSpace(latest) ? "未知" : latest;
        CurrentText.Text = AppInfo.Version;
        if (minVersion is not null)
        {
            ReasonText.Text = $"你的版本 {AppInfo.Version} 低于允许的最低版本 {minVersion},已被服务端停用。必须更新后才能继续使用。";
        }

        if (string.IsNullOrWhiteSpace(downloadUrl))
        {
            DownloadButton.IsEnabled = false;
            DownloadButton.Content = "未配置下载链接";
        }
        else
        {
            DownloadButton.Click += (_, _) =>
            {
                try
                {
                    Process.Start(new ProcessStartInfo(downloadUrl) { UseShellExecute = true });
                }
                catch
                {
                    // 打开失败不阻塞退出。
                }

                Close();
            };
        }

        QuitButton.Click += (_, _) => Close();
        CloseButton.Click += (_, _) => Close();
    }
}
