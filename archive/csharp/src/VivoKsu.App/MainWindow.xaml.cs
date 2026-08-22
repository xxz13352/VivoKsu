using System.Collections.Specialized;
using System.Windows;
using System.Windows.Input;
using System.Windows.Threading;
using VivoKsu.App.Services;

namespace VivoKsu.App;

public partial class MainWindow : Window
{
    private readonly AppComposition composition;

    public MainWindow(AppComposition composition)
    {
        InitializeComponent();
        this.composition = composition;
        DataContext = composition.MainViewModel;
        Loaded += OnWindowLoaded;
        Closed += OnWindowClosed;
    }

    private async void OnWindowLoaded(object sender, RoutedEventArgs eventArgs)
    {
        await composition.StartAsync();

        // 操作日志新增条目时自动滚动到底部(刷机日志持续输出)。
        var entries = composition.MainViewModel.Logs.Entries;
        entries.CollectionChanged += (_, args) =>
        {
            if (args.Action != NotifyCollectionChangedAction.Add)
            {
                return;
            }

            Dispatcher.BeginInvoke(DispatcherPriority.Background, new Action(() =>
            {
                if (OperationLogList is not null && entries.Count > 0)
                {
                    OperationLogList.ScrollIntoView(entries[^1]);
                }
            }));
        };
    }

    private void OnWindowClosed(object? sender, EventArgs eventArgs)
    {
        // Cleanup is owned by App.OnExit, which blocks until it completes; an
        // async-void StopAsync here would be cut off as the process exits.
    }

    private void OnTitleBarMouseLeftButtonDown(object sender, MouseButtonEventArgs eventArgs)
    {
        if (eventArgs.ChangedButton != MouseButton.Left)
        {
            return;
        }

        if (eventArgs.ClickCount == 2)
        {
            ToggleMaximize();
            return;
        }

        DragMove();
    }

    private void OnMinimizeClick(object sender, RoutedEventArgs eventArgs) => WindowState = WindowState.Minimized;

    private void OnToggleMaximizeClick(object sender, RoutedEventArgs eventArgs) => ToggleMaximize();

    private void OnCloseClick(object sender, RoutedEventArgs eventArgs) => Close();

    private void ToggleMaximize() => WindowState = WindowState == WindowState.Maximized ? WindowState.Normal : WindowState.Maximized;
}
