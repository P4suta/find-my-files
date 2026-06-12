using CommunityToolkit.Mvvm.ComponentModel;
using FindMyFiles;
using FindMyFiles.Engine;
using FindMyFiles.Services;

namespace FindMyFiles.ViewModels;

/// <summary>
/// Composition root of the main page: owns the UI state (search text, sort,
/// filter, status line) and the user-facing wording for failures, and wires
/// the focused components together — <see cref="SearchOrchestrator"/> (when
/// to search), <see cref="ResultsPresenter"/> (how results land),
/// <see cref="NotificationCenter"/> (InfoBar stack) and
/// <see cref="PerfPanelViewModel"/> (F12).
/// </summary>
public sealed partial class MainViewModel : ObservableObject
{
    private readonly IEngineClient _engine;

    /// <summary>The one place engine events cross onto the UI thread —
    /// every handler below already runs dispatched.</summary>
    private readonly EngineEventMarshaler _engineEvents;

    [ObservableProperty]
    public partial string SearchText { get; set; } = string.Empty;

    [ObservableProperty]
    public partial string StatusText { get; set; } = "準備中…";

    [ObservableProperty]
    public partial FmfSort Sort { get; set; } = FmfSort.Name;

    [ObservableProperty]
    public partial bool SortDescending { get; set; }

    [ObservableProperty]
    public partial bool IncludeHiddenSystem { get; set; }

    /// <summary>絞り込みモード (focused search, ADR-0019): the toolbar toggle.
    /// Initialized from settings in the ctor; flips push down to the
    /// orchestrator, persist, and requery as a filter change (top reset).</summary>
    [ObservableProperty]
    public partial bool FocusedSearch { get; set; }

    /// <summary>Status-bar badge: which engine transport is active
    /// (サービス接続 / 再接続中… / 管理者(in-proc) / fake).</summary>
    [ObservableProperty]
    public partial string EngineModeText { get; set; } = string.Empty;

    public ResultsPresenter Results { get; }
    public SearchOrchestrator Search { get; }
    public NotificationCenter Notifications { get; }
    public PerfPanelViewModel Perf { get; }

    private readonly AppSettings _settings;

    public MainViewModel(IEngineClient engine, IDispatcher dispatcher, AppSettings? settings = null)
    {
        _engine = engine;
        _settings = settings ?? AppSettings.Load();
        _engineEvents = new EngineEventMarshaler(engine, dispatcher);
        Results = new ResultsPresenter(dispatcher);
        Search = new SearchOrchestrator(engine, _engineEvents, dispatcher, Results,
            () => new SearchRequest(
                SearchText,
                new SearchOptions(Sort, SortDescending, FmfCase.Smart, IncludeHiddenSystem)));
        // Focused-search wiring: the lists are settings-owned; the toggle
        // state flows through OnFocusedSearchChanged (Search exists by now).
        Search.FocusedExcludePaths = _settings.FocusedExcludePaths;
        Search.FocusedExtensions = _settings.FocusedExtensions;
        FocusedSearch = _settings.FocusedSearch;
        Notifications = new NotificationCenter(dispatcher);
        Perf = new PerfPanelViewModel(engine);

        Search.TraceCaptured += Perf.RecordTrace;
        Search.SearchFailed += OnSearchFailed;

        _engineEvents.VolumeUpdated += OnVolumeUpdated;
        _engineEvents.EngineErrorOccurred += severity =>
            HandleEngineErrorAsync(severity).Forget("engine.error");
        _engineEvents.ConnectionChanged += OnConnectionChanged;
        EngineModeText = StatusFormatter.EngineMode(_engine);

        Notifications.AttachToNotifier();
    }

    /// <summary>The single persistent banner while the pipe reconnects —
    /// held by reference so it never duplicates and is removed on recovery.
    /// Non-Info notifications never auto-dissolve (NotificationCenter).</summary>
    private AppNotification? _reconnectBanner;

    private void OnConnectionChanged(EngineConnectionState state)
    {
        EngineModeText = StatusFormatter.EngineMode(_engine);
        if (state == EngineConnectionState.Reconnecting)
        {
            if (_reconnectBanner is null)
            {
                _reconnectBanner = new AppNotification(
                    NotifySeverity.Warning,
                    "エンジンサービスに再接続しています…",
                    "接続が回復すると結果は自動的に更新されます");
                Notifications.Push(_reconnectBanner);
            }
        }
        else if (state == EngineConnectionState.Connected && _reconnectBanner is not null)
        {
            Notifications.Remove(_reconnectBanner);
            _reconnectBanner = null;
        }
    }

    /// <summary>Startup sequence, in order: status text → StartIndexing →
    /// initial requery. Runs on the UI thread; the engine calls are awaited
    /// so a pipe transport never blocks it.</summary>
    public async Task StartAsync()
    {
        if (_engine is FakeEngineClient { IsEmpty: true })
        {
            // Unelevated, no service: the factory's notification explains
            // the way out; the status line must not pretend to index.
            StatusText = "未接続 — 通知の手順で検索サービスをセットアップしてください";
            return;
        }
        try
        {
            var volumes = await _engine.ListVolumesAsync();
            StatusText = StatusFormatter.IndexingStarted(volumes);
            await _engine.StartIndexingAsync(volumes);
        }
        catch (Exception ex)
        {
            FileLog.Error("engine", "startup indexing failed", ex);
            StatusText = "インデックス開始に失敗しました";
            Notifications.Push(new AppNotification(
                NotifySeverity.Error, "インデックスを開始できませんでした", ex.Message));
        }
        OfferServiceSetup();
        Search.Requery(RequeryOrigin.Initial);
    }

    /// <summary>Elevated in-proc session with the service absent or stopped →
    /// offer the one-click setup (the GUI half ADR-0016 left to a terminal).
    /// The usage story this completes: plain double-click → 「管理者として
    /// 再起動」 → this button, once — then plain double-click forever.</summary>
    private void OfferServiceSetup()
    {
        // On the pipe → the service already accepts us. Not elevated → can't
        // install/restart anyway (the factory already pointed the way out).
        // Either way there is nothing to offer.
        if (_engine is PipeEngineClient || !ServiceSetup.IsProcessElevated())
        {
            return;
        }
        var exe = ServiceSetup.LocateServiceExe(AppContext.BaseDirectory);
        if (exe is null)
        {
            FileLog.Warn("service-setup", "fmf-service.exe not found — setup offer suppressed");
            return;
        }
        var state = ServiceSetup.QueryState();
        if (state == EngineServiceState.Running)
        {
            // Elevated, yet not on the pipe → the running service rejected
            // this user (a stale authorized-SID list baked in at its startup).
            // Re-register: install appends our SID (plus any forwarded owner)
            // and restart applies it — the recovery path out of the lockout.
            Notifications.Push(new AppNotification(
                NotifySeverity.Warning,
                "このユーザーを検索サービスに登録し直します",
                "「登録し直す」を押すとあなたのアカウントが接続を許可され、サービスが"
                + "再起動して反映されます。以後は通常起動でそのまま検索できます。",
                "登録し直す",
                () => RunServiceSetupAsync(exe).Forget("service-setup")));
            return;
        }
        Notifications.Push(new AppNotification(
            NotifySeverity.Warning,
            state == EngineServiceState.NotInstalled
                ? "管理者モードで動作中です — 検索サービスを登録できます"
                : "検索サービスは登録済みですが停止しています",
            "サービスを開始しておくと、次回からは通常起動(ダブルクリック)でそのまま検索できます。",
            state == EngineServiceState.NotInstalled ? "サービスを登録して開始" : "サービスを開始",
            () => RunServiceSetupAsync(exe).Forget("service-setup")));
    }

    private async Task RunServiceSetupAsync(string exe)
    {
        var (ok, transcript) = await Task.Run(
            () => ServiceSetup.InstallAndRestart(exe, App.SetupOwnerSid));
        FileLog.Info("service-setup", transcript);
        Notifications.Push(ok
            ? new AppNotification(
                NotifySeverity.Info,
                "検索サービスを設定しました",
                "サービスが最新の許可設定で再起動しました。次回からは通常起動"
                + "(ダブルクリック)でそのまま検索できます。")
            : new AppNotification(
                NotifySeverity.Error,
                "サービスのセットアップに失敗しました",
                Truncate(transcript, 300)));
    }

    partial void OnSearchTextChanged(string value) => Search.NotifyTextChanged(value);

    partial void OnIncludeHiddenSystemChanged(bool value) =>
        Search.Requery(RequeryOrigin.Filter);

    /// <summary>Toggle → orchestrator + persistence + filter requery. Also
    /// runs once from the ctor (settings=true flips the default-false
    /// property): the save is skipped (no change) and the requery is a no-op
    /// on the still-empty query.</summary>
    partial void OnFocusedSearchChanged(bool value)
    {
        Search.FocusedSearch = value;
        if (_settings.FocusedSearch != value)
        {
            _settings.FocusedSearch = value;
            _settings.Save();
        }
        Search.Requery(RequeryOrigin.Filter);
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
        Search.Requery(RequeryOrigin.Sort);
    }

    private void OnVolumeUpdated(VolumeStatus s)
    {
        StatusText = StatusFormatter.Volume(s, StatusText);
        if (s.State == VolumeState.Failed)
        {
            Notifications.Push(new AppNotification(
                NotifySeverity.Error,
                $"{s.Label} のインデックスに失敗しました",
                "詳細は F12 パネルまたは engine.log を参照"));
        }
        if (s.State == VolumeState.Ready)
        {
            Search.Requery(RequeryOrigin.VolumeReady);
        }
    }

    private void OnSearchFailed(Exception e)
    {
        if (_engine.Connection == EngineConnectionState.Reconnecting)
        {
            return; // the persistent reconnect banner already explains this
        }
        Notifications.Push(new AppNotification(
            NotifySeverity.Error,
            e is EngineException ? "検索に失敗しました" : "検索中に予期しないエラーが発生しました",
            e.Message));
    }

    /// <summary>Engine diagnostics: pull the detail text behind the POD event.</summary>
    private async Task HandleEngineErrorAsync(int severity)
    {
        await Perf.RefreshStatsAsync();
        if (severity >= 2)
        {
            var last = Perf.Stats?.RecentErrors.LastOrDefault();
            Notifications.Push(new AppNotification(
                NotifySeverity.Error,
                severity >= 3 ? "エンジン内部でパニックが発生しました" : "エンジンでエラーが発生しました",
                last is null ? null : $"[{last.Area}] {Truncate(last.Message, 200)}"));
        }
    }

    private static string Truncate(string s, int max) =>
        s.Length <= max ? s : s[..max] + "…";
}
