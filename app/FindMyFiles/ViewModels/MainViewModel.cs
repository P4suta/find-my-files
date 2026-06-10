using System.Collections.ObjectModel;
using CommunityToolkit.Mvvm.ComponentModel;
using Microsoft.UI.Dispatching;
using FindMyFiles.Engine;
using FindMyFiles.Services;
using FindMyFiles.Virtualization;

namespace FindMyFiles.ViewModels;

/// <summary>
/// Search pipeline: TextChanged → 50ms debounce → engine query → swap the
/// virtualized ItemsSource, with a generation counter discarding stale
/// responses (docs/ARCHITECTURE.md). Also owns the InfoBar notification
/// stack — every error path in the app funnels through here.
/// </summary>
public sealed partial class MainViewModel : ObservableObject
{
    private readonly IEngineClient _engine;
    private readonly DispatcherQueue _dispatcher;
    private readonly DispatcherQueueTimer _debounce;
    private long _generation;

    [ObservableProperty]
    public partial string SearchText { get; set; } = string.Empty;

    [ObservableProperty]
    public partial VirtualResultList? ResultsSource { get; set; }

    [ObservableProperty]
    public partial string StatusText { get; set; } = "準備中…";

    [ObservableProperty]
    public partial string CountText { get; set; } = string.Empty;

    [ObservableProperty]
    public partial FmfSort Sort { get; set; } = FmfSort.Name;

    [ObservableProperty]
    public partial bool SortDescending { get; set; }

    [ObservableProperty]
    public partial bool IncludeHiddenSystem { get; set; }

    partial void OnIncludeHiddenSystemChanged(bool value) =>
        RunQueryAsync().Forget("query.toggle");

    // ── Notifications (InfoBar stack) ───────────────────────────────────

    public ObservableCollection<AppNotification> Notifications { get; } = [];

    private const int MaxNotifications = 3;

    private void PushNotification(AppNotification n)
    {
        while (Notifications.Count >= MaxNotifications)
        {
            Notifications.RemoveAt(0);
        }
        Notifications.Add(n);
        if (n.Severity == NotifySeverity.Info)
        {
            var timer = _dispatcher.CreateTimer();
            timer.Interval = TimeSpan.FromSeconds(5);
            timer.IsRepeating = false;
            timer.Tick += (_, _) => Notifications.Remove(n);
            timer.Start();
        }
    }

    public void RemoveNotification(AppNotification n) => Notifications.Remove(n);

    /// <summary>Engine diagnostics: pull the detail text behind the POD event.</summary>
    private async Task HandleEngineErrorAsync(int severity)
    {
        Stats = await _engine.GetStatsAsync();
        PerfDataChanged?.Invoke();
        if (severity >= 2)
        {
            var last = Stats?.RecentErrors.LastOrDefault();
            PushNotification(new AppNotification(
                NotifySeverity.Error,
                severity >= 3 ? "エンジン内部でパニックが発生しました" : "エンジンでエラーが発生しました",
                last is null ? null : $"[{last.Area}] {Truncate(last.Message, 200)}"));
        }
    }

    private static string Truncate(string s, int max) =>
        s.Length <= max ? s : s[..max] + "…";

    // ── Performance panel (F12) ─────────────────────────────────────────

    [ObservableProperty]
    public partial bool IsPerfPanelOpen { get; set; }

    [ObservableProperty]
    public partial QueryTraceData? LastTrace { get; set; }

    [ObservableProperty]
    public partial EngineStatsData? Stats { get; set; }

    private readonly List<ulong> _recentTotalsUs = [];

    /// <summary>Latencies of the most recent queries (µs, oldest first).</summary>
    public IReadOnlyList<ulong> RecentTotalsUs => _recentTotalsUs;

    /// <summary>Raised on the UI thread whenever trace/stats data moved.</summary>
    public event Action? PerfDataChanged;

    public void TogglePerfPanel() => IsPerfPanelOpen = !IsPerfPanelOpen;

    public async Task RefreshStatsAsync()
    {
        Stats = await _engine.GetStatsAsync();
        PerfDataChanged?.Invoke();
    }

    // ── Lifecycle & search pipeline ─────────────────────────────────────

    public MainViewModel(IEngineClient engine, DispatcherQueue dispatcher)
    {
        _engine = engine;
        _dispatcher = dispatcher;
        _debounce = dispatcher.CreateTimer();
        _debounce.Interval = TimeSpan.FromMilliseconds(50);
        _debounce.IsRepeating = false;
        _debounce.Tick += (_, _) => RunQueryAsync().Forget("query.debounce");

        _engine.IndexChanged += volume =>
            _dispatcher.TryEnqueue(() => RunQueryAsync().Forget("query.index-changed"));
        _engine.VolumeUpdated += s => _dispatcher.TryEnqueue(() => OnVolumeUpdated(s));
        _engine.EngineErrorOccurred += severity =>
            _dispatcher.TryEnqueue(() =>
                HandleEngineErrorAsync(severity).Forget("engine.error"));

        Notifier.Attach(n => _dispatcher.TryEnqueue(() => PushNotification(n)));
    }

    public void Start()
    {
        try
        {
            var volumes = _engine.ListVolumes();
            StatusText = volumes.Count == 0
                ? "NTFS固定ドライブが見つかりません"
                : $"インデックス作成中: {string.Join(", ", volumes)}";
            _engine.StartIndexing(volumes);
        }
        catch (Exception ex)
        {
            FileLog.Error("engine", "startup indexing failed", ex);
            StatusText = "インデックス開始に失敗しました";
            PushNotification(new AppNotification(
                NotifySeverity.Error, "インデックスを開始できませんでした", ex.Message));
        }
        RunQueryAsync().Forget("query.initial");
    }

    partial void OnSearchTextChanged(string value)
    {
        _debounce.Stop();
        if (string.IsNullOrEmpty(value))
        {
            RunQueryAsync().Forget("query.clear"); // clearing should feel instant
        }
        else
        {
            _debounce.Start();
        }
    }

    public void SetSort(FmfSort key)
    {
        if (Sort == key)
        {
            SortDescending = !SortDescending;
        }
        else
        {
            Sort = key;
            SortDescending = false;
        }
        RunQueryAsync().Forget("query.sort");
    }

    private void OnVolumeUpdated(VolumeStatus s)
    {
        StatusText = s.State switch
        {
            VolumeState.Scanning => $"{s.Label} をインデックス中… {s.Entries:N0} 件",
            VolumeState.Ready => $"{s.Label} 準備完了 — {s.Entries:N0} 件",
            VolumeState.Rescanning => $"{s.Label} を再スキャン中…",
            VolumeState.Failed => $"{s.Label} のインデックスに失敗",
            _ => StatusText,
        };
        if (s.State == VolumeState.Failed)
        {
            PushNotification(new AppNotification(
                NotifySeverity.Error,
                $"{s.Label} のインデックスに失敗しました",
                "詳細は F12 パネルまたは engine.log を参照"));
        }
        if (s.State == VolumeState.Ready)
        {
            RunQueryAsync().Forget("query.volume-ready");
        }
    }

    private async Task RunQueryAsync()
    {
        var generation = Interlocked.Increment(ref _generation);
        var query = SearchText;
        var options = new SearchOptions(Sort, SortDescending, FmfCase.Smart, IncludeHiddenSystem);
        try
        {
            var outcome = await _engine.SearchAsync(query, options);
            var result = outcome.Result;
            if (generation != Interlocked.Read(ref _generation))
            {
                result.Dispose(); // a newer query superseded this one
                return;
            }
            var old = ResultsSource;
            var list = new VirtualResultList(result, _dispatcher);
            list.BecameStale += () => RunQueryAsync().Forget("query.stale");
            ResultsSource = list;
            old?.Dispose();

            LastTrace = outcome.Trace;
            if (outcome.Trace is { } t)
            {
                _recentTotalsUs.Add(t.TotalUs);
                if (_recentTotalsUs.Count > 64)
                {
                    _recentTotalsUs.RemoveAt(0);
                }
                CountText = $"{t.TotalUs / 1000.0:F1} ms · {result.Count:N0} 件";
            }
            else
            {
                CountText = $"{result.Count:N0} 件";
            }
            PerfDataChanged?.Invoke();
        }
        catch (QuerySyntaxException e)
        {
            CountText = $"クエリエラー: {e.Message}";
        }
        catch (EngineException e)
        {
            FileLog.Error("query", $"engine error for query `{query}`", e);
            CountText = string.Empty;
            PushNotification(new AppNotification(
                NotifySeverity.Error, "検索に失敗しました", e.Message));
        }
        catch (Exception e)
        {
            // Last line of defense: never let a query crash the app silently.
            FileLog.Error("query", $"unexpected failure for query `{query}`", e);
            PushNotification(new AppNotification(
                NotifySeverity.Error, "検索中に予期しないエラーが発生しました", e.Message));
        }
    }
}
