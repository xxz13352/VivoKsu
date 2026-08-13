using System.Windows;
using System.Windows.Input;
using VivoKsu.App.Services;

namespace VivoKsu.App;

/// <summary>启动登录门禁:账号+密码验证通过才进入主界面。</summary>
public partial class LoginWindow : Window
{
    private readonly ToolPathPreferences preferences;
    private readonly LoginService loginService;

    public LoginWindow(ToolPathPreferences preferences, LoginService loginService)
    {
        InitializeComponent();
        this.preferences = preferences;
        this.loginService = loginService;

        UsernameBox.Text = preferences.Username ?? string.Empty;
        if (string.IsNullOrWhiteSpace(UsernameBox.Text))
        {
            UsernameBox.Focus();
        }
        else
        {
            PasswordBox.Focus();
        }

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

        LoginButton.IsEnabled = false;
        ErrorText.Text = "正在登录…";
        try
        {
            var result = await loginService.LoginAsync(username, password, CancellationToken.None);
            Token = result.Token;
            Username = result.Username;
            if (RememberBox.IsChecked == true)
            {
                preferences.SaveCredentials(result.Username, result.Token);
            }
            else
            {
                preferences.ClearCredentials();
            }

            DialogResult = true;
        }
        catch (LoginFailedException exception)
        {
            ErrorText.Text = exception.Message;
        }
        catch (Exception)
        {
            ErrorText.Text = "无法连接服务器,请检查网络后重试。";
        }
        finally
        {
            LoginButton.IsEnabled = true;
        }
    }
}
