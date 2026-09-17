using System.Collections.ObjectModel;
using System.Windows.Threading;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using VivoKsu.App.Models;
using VivoKsu.App.Services;

namespace VivoKsu.App.ViewModels;

/// <summary>
/// 在线状态页:轮询 GET /api/online 展示在线用户、在线时长与心跳健康度。
/// 单一 1s DispatcherTimer 驱动:每 5 个 tick 拉一次列表、每个 tick 刷新时长文本;
/// 所有 ObservableCollection 改动都在 UI 线程(await 后回到 Dispatcher 上下文),不做跨线程修改。
/// 轮询不依赖页面是否可见(登录即启动,1 请求/5s 负载可忽略)。
/// </summary>
public sealed partial class OnlineViewModel : ObservableObject, IDisposable
{
    private const int RefreshEveryTicks = 5;
    private readonly OtaApiClient client;
    private readonly HeartbeatService heartbeat;
    private DispatcherTimer? timer;
    private int tickCount;
    private bool refreshInProgress;

    public OnlineViewModel(OtaApiClient client, HeartbeatService heartbeat)
    {
        this.client = client;
        this.heartbeat = heartbeat;
        RefreshCommand = new AsyncRelayCommand(() => RefreshAsync());
    }

    public ObservableCollection<OnlineSessionItem> Sessions { get; } = [];

    public IAsyncRelayCommand RefreshCommand { get; }

    [ObservableProperty]
    private int onlineCount;

    [ObservableProperty]
    private string lastUpdatedText = "尚未刷新";

    [ObservableProperty]
    private string statusText = "登录后自动同步在线状态。";

    [ObservableProperty]
    private string heartbeatHealthText = "心跳:未连接";

    /// <summary>启动 1s 定时器(必须由 UI 线程调用;App 在登录成功后调用)。重复调用为 no-op。</summary>
    public void Start()
    {
        if (timer is not null)
        {
            return;
        }

        var created = new DispatcherTimer { Interval = TimeSpan.FromSeconds(1) };
        created.Tick += OnTick;
        timer = created;
        created.Start();
        _ = RefreshAsync();
    }

    /// <summary>停止定时器并丢弃在途轮询(由 AppComposition.StopAsync 调用)。幂等。</summary>
    public void Stop()
    {
        if (timer is not null)
        {
            timer.Stop();
            timer = null;
        }

        refreshInProgress = false;
    }

    private void OnTick(object? sender, EventArgs e)
    {
        UpdateHeartbeatHealth();
        RefreshDurations();
        if (++tickCount % RefreshEveryTicks == 0)
        {
            _ = RefreshAsync();
        }
    }

    private void UpdateHeartbeatHealth()
    {
        HeartbeatHealthText = heartbeat.IsRunning
            ? heartbeat.IsHealthy ? "心跳:正常" : "心跳:异常"
            : "心跳:未连接";
    }

    private void RefreshDurations()
    {
        var now = DateTimeOffset.UtcNow.ToUnixTimeSeconds();
        foreach (var item in Sessions)
        {
            item.UpdateDuration(now);
        }
    }

    private async Task RefreshAsync()
    {
        if (refreshInProgress)
        {
            return;
        }

        refreshInProgress = true;
        try
        {
            // 刻意不用 ConfigureAwait(false):延续回到 UI 线程,直接改 ObservableCollection。
            var sessions = await client.GetOnlineAsync(CancellationToken.None);
            var now = DateTimeOffset.UtcNow.ToUnixTimeSeconds();
            Sessions.Clear();
            foreach (var session in sessions)
            {
                Sessions.Add(new OnlineSessionItem(session, now));
            }

            OnlineCount = sessions.Count;
            LastUpdatedText = $"更新于 {DateTime.Now:HH:mm:ss}";
            StatusText = sessions.Count == 0
                ? "当前没有在线用户。"
                : $"{sessions.Count} 个在线会话";
        }
        catch (UpdateRequiredException)
        {
            // 426 由心跳循环单独路由到强制更新窗;这里静默,避免重复弹窗。
        }
        catch (OtaApiException exception) when (exception.StatusCode is 401 or 403)
        {
            StatusText = "账号已停用或登录失效。";
        }
        catch (Exception)
        {
            StatusText = "无法连接服务器,在线状态已过期。";
        }
        finally
        {
            refreshInProgress = false;
        }
    }

    public void Dispose() => Stop();
}

/// <summary>单个在线会话的显示模型:时长文本每秒刷新。</summary>
public sealed class OnlineSessionItem : ObservableObject
{
    private readonly long connectedEpochSeconds;
    private string durationText = "00:00";

    public OnlineSessionItem(OnlineSession session, long nowEpochSeconds)
    {
        Name = session.Name;
        ClientVersion = session.ClientVersion;
        IsSelf = session.IsSelf;
        connectedEpochSeconds = session.ConnectedAtEpochSeconds;
        UpdateDuration(nowEpochSeconds);
    }

    public string Name { get; }

    public string ClientVersion { get; }

    public bool IsSelf { get; }

    public string DurationText
    {
        get => durationText;
        private set => SetProperty(ref durationText, value);
    }

    public void UpdateDuration(long nowEpochSeconds)
    {
        var elapsed = TimeSpan.FromSeconds(Math.Max(0, nowEpochSeconds - connectedEpochSeconds));
        DurationText = $"{(int)elapsed.TotalHours:D2}:{elapsed.Minutes:D2}:{elapsed.Seconds:D2}";
    }
}
