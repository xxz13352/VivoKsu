using System.Windows;
using System.Windows.Input;
using VivoKsu.App.Services;

namespace VivoKsu.App;

/// <summary>启动登录门禁:每次启动强制账号+密码登录,验证通过才进入主界面。</summary>
public partial class LoginWindow : Window
{
    private readonly LoginService loginService;

    public LoginWindow(LoginService loginService)
    {
        InitializeComponent();
        this.loginService = loginService;
        UsernameBox.Focus();

        LoginButton.Click += async (_, _) => await LoginAsync();
        PasswordBox.KeyDown += (_, e) =>
        {
            if (e.Key == Key.Enter)
            {
                _ = LoginAsync();
            }
        };
        UsernameBox.KeyDown += (_, e) =>
        {
            if (e.Key == Key.Enter)
            {
                PasswordBox.Focus();
            }
        };
        CloseButton.Click += (_, _) => Close();
        KeyDown += (_, e) =>
        {
            if (e.Key == Key.Escape)
            {
                Close();
            }
        };
        MouseLeftButtonDown += (_, e) =>
        {
            if (e.LeftButton == MouseButtonState.Pressed)
            {
                DragMove();
            }
        };
    }

    /// <summary>密码显示/隐藏:眼睛按钮勾选则明文,取消则恢复 ●。Click 触发时 IsChecked 已是新值。</summary>
    private void OnPasswordToggleClick(object sender, RoutedEventArgs e)
        => PasswordBox.PasswordChar = PasswordToggleButton.IsChecked == true ? '\0' : '●';

    /// <summary>登录成功后的 API token(供 App 传给 OtaApiClient)。</summary>
    public string? Token { get; private set; }

    public string? Username { get; private set; }

    private async Task LoginAsync()
    {
        var username = UsernameBox.Text.Trim();
        var password = PasswordBox.Password;
        if (string.IsNullOrWhiteSpace(username) || string.IsNullOrEmpty(password))
        {
            ErrorText.Text = "请输入账号和密码。";
            return;
        }

        // 登录中:禁用交互 + 覆盖层提示,防止重复提交。
        LoginButton.IsEnabled = false;
        ErrorText.Text = string.Empty;
        BusyOverlay.Visibility = Visibility.Visible;
        try
        {
            var result = await loginService.LoginAsync(username, password, CancellationToken.None);
            Token = result.Token;
            Username = result.Username;
            DialogResult = true;
        }
        catch (LoginFailedException exception)
        {
            ErrorText.Text = exception.Message;
        }
        catch (UpdateRequiredException)
        {
            // 版本过低 → 426:冒泡给 App 全局处理器弹强制更新窗。
            throw;
        }
        catch (Exception)
        {
            ErrorText.Text = "无法连接服务器，请检查网络后重试。";
        }
        finally
        {
            BusyOverlay.Visibility = Visibility.Collapsed;
            LoginButton.IsEnabled = true;
            PasswordBox.Focus();
        }
    }
}
