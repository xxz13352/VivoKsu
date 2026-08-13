using System.Windows;
using System.Windows.Threading;
using VivoKsu.App.Services;

namespace VivoKsu.App;

public partial class App : Application
{
    private AppComposition? composition;

    protected override void OnStartup(StartupEventArgs eventArgs)
    {
        base.OnStartup(eventArgs);
        composition = AppComposition.CreateDefault();
        var mainWindow = new MainWindow(composition);
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
