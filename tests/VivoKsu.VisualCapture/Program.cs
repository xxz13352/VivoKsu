using System.IO;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;
using System.Windows.Media.Imaging;
using System.Windows.Threading;
using VivoKsu.App;
using VivoKsu.App.Models;
using VivoKsu.App.Services;
using WpfApplication = VivoKsu.App.App;

namespace VivoKsu.VisualCapture;

internal static class Program
{
    [STAThread]
    private static int Main(string[] args)
    {
        if (!TryParseArguments(args, out var page, out var outputPath))
        {
            Console.Error.WriteLine("Usage: VivoKsu.VisualCapture --page <AppPage> --output <absolute PNG path>");
            return 2;
        }

        var application = new WpfApplication();
        application.InitializeComponent();
        var composition = AppComposition.CreateForTesting(new EmptyNativeApi(), new EmptyProcessRunner());
        composition.MainViewModel.SelectedPage = page;
        var window = new MainWindow(composition);
        var exitCode = 1;

        window.Loaded += async (_, _) =>
        {
            try
            {
                await WaitForImageDecodeAsync(window);
                window.UpdateLayout();
                SaveWindow(window, outputPath);
                exitCode = 0;
            }
            catch (Exception exception)
            {
                Console.Error.WriteLine(exception.Message);
            }
            finally
            {
                await composition.StopAsync();
                window.Close();
                application.Shutdown();
                Dispatcher.CurrentDispatcher.InvokeShutdown();
            }
        };

        window.Show();
        Dispatcher.Run();
        return exitCode;
    }

    private static bool TryParseArguments(string[] args, out AppPage page, out string outputPath)
    {
        page = AppPage.Overview;
        outputPath = string.Empty;
        for (var index = 0; index < args.Length - 1; index++)
        {
            if (args[index] == "--page" && Enum.TryParse(args[index + 1], ignoreCase: true, out AppPage parsedPage))
            {
                page = parsedPage;
            }

            if (args[index] == "--output")
            {
                outputPath = args[index + 1];
            }
        }

        return Path.IsPathFullyQualified(outputPath)
            && string.Equals(Path.GetExtension(outputPath), ".png", StringComparison.OrdinalIgnoreCase);
    }

    private static async Task WaitForImageDecodeAsync(DependencyObject root)
    {
        for (var attempt = 0; attempt < 20; attempt++)
        {
            var images = FindDescendants<Image>(root).ToArray();
            if (images.All(image => image.Source is BitmapSource { PixelWidth: > 0, PixelHeight: > 0 }))
            {
                return;
            }

            await Task.Delay(50);
        }

        var imageStates = string.Join(", ", FindDescendants<Image>(root).Select(image => image.Source switch
        {
            BitmapSource bitmap => $"BitmapSource {bitmap.PixelWidth}x{bitmap.PixelHeight}",
            null => "null source",
            _ => image.Source.GetType().Name
        }));
        throw new InvalidOperationException($"WPF 品牌资源未完成解码，拒绝生成不完整视觉基线。图像状态: {imageStates}");
    }

    private static IEnumerable<T> FindDescendants<T>(DependencyObject root) where T : DependencyObject
    {
        for (var index = 0; index < VisualTreeHelper.GetChildrenCount(root); index++)
        {
            var child = VisualTreeHelper.GetChild(root, index);
            if (child is T typed)
            {
                yield return typed;
            }

            foreach (var descendant in FindDescendants<T>(child))
            {
                yield return descendant;
            }
        }
    }

    private static void SaveWindow(Window window, string outputPath)
    {
        var width = (int)Math.Ceiling(window.ActualWidth);
        var height = (int)Math.Ceiling(window.ActualHeight);
        if (width <= 0 || height <= 0)
        {
            throw new InvalidOperationException("WPF 主窗口未完成布局，无法捕获视觉基线。");
        }

        var bitmap = new RenderTargetBitmap(width, height, 96, 96, PixelFormats.Pbgra32);
        bitmap.Render(window);
        Directory.CreateDirectory(Path.GetDirectoryName(outputPath)!);
        using var stream = File.Create(outputPath);
        var encoder = new PngBitmapEncoder();
        encoder.Frames.Add(BitmapFrame.Create(bitmap));
        encoder.Save(stream);
    }

    private sealed class EmptyNativeApi : IFastbootRsNativeApi
    {
        public string ListDevices() => string.Empty;
        public string Shell(string? serial, string command, int timeoutMilliseconds = 15000) => string.Empty;
        public string GetVar(string? serial, string variable) => string.Empty;
        public void Reboot(string? serial, string target) { }
        public void FastbootReboot(string? serial, string? target) { }
        public void SetActive(string? serial, string slot) { }
        public void Push(string? serial, string localPath, string remotePath, int timeoutMilliseconds = 15000) { }
        public long Pull(string? serial, string remotePath, string localPath, int timeoutMilliseconds = 15000) => 0;
        public string Install(string? serial, string apkPath, bool replace) => string.Empty;
        public void Flash(string? serial, string partition, string imagePath) { }
        public void Erase(string? serial, string partition) { }
        public long Fetch(string? serial, string partition, string outputPath) => 0;
    }

    private sealed class EmptyProcessRunner : IProcessRunner
    {
        public IRunningProcess Start(
            string executable,
            IReadOnlyList<string> arguments,
            IReadOnlyDictionary<string, string>? environment = null) => new EmptyRunningProcess();
    }

    private sealed class EmptyRunningProcess : IRunningProcess
    {
        public bool HasExited => true;
        public event EventHandler? Exited;
        public void Stop() => Exited?.Invoke(this, EventArgs.Empty);
        public void Dispose() { }
    }
}
