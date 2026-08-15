using System.Windows;
using System.Windows.Input;
using VivoKsu.App.ViewModels;

namespace VivoKsu.App;

/// <summary>
/// 「组件安装」液态玻璃窗:检测缺失资源 → 并行下载 → 就绪/失败。
/// 纯 UI;逻辑在 <see cref="ResourceDownloadViewModel"/>。由登录后检测或软件页「安装组件」打开。
/// </summary>
public partial class ResourceDownloadWindow : Window
{
    private readonly ResourceDownloadViewModel viewModel;

    public ResourceDownloadWindow(ResourceDownloadViewModel viewModel)
    {
        InitializeComponent();
        this.viewModel = viewModel;
        DataContext = viewModel;
        // 全部完成或跳过 → 关窗;取消 → 关窗。DialogResult 供调用方区分(当前未用)。
        viewModel.OnFinished = finished =>
        {
            DialogResult = finished;
            Close();
        };
    }

    protected override void OnClosed(EventArgs e)
    {
        // 窗口被直接关闭(如点 ✕)时取消在途下载,避免下载在窗死后无谓继续。
        viewModel.Cancel();
        base.OnClosed(e);
    }

    private void OnCloseClick(object sender, RoutedEventArgs e)
    {
        DialogResult = false;
        Close();
    }

    private void OnTitleBarMouseDown(object sender, MouseButtonEventArgs e)
    {
        if (e.LeftButton == MouseButtonState.Pressed)
        {
            DragMove();
        }
    }
}
