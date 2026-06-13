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
    public partial string StatusText { get; set; } = Loc.Get("Status_Preparing");

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

    // ── Disconnected setup screen (empty fake engine: no service yet) ──

    /// <summary>True when the engine is the empty fake (unelevated, no service)
    /// — the page shows the setup screen instead of a search box that can only
    /// return zero rows. Fixed for this instance's lifetime (the transport is
    /// chosen once; registering relaunches), so x:Bind OneTime is enough.</summary>
    public bool IsDisconnected => _engine is FakeEngineClient { IsEmpty: true };

    public bool IsReady => !IsDisconnected;

    /// <summary>Setup screen progress text ("管理者の許可を待っています…" etc.);
    /// empty hides the progress row.</summary>
    [ObservableProperty]
    public partial string SetupStatus { get; set; } = string.Empty;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(SetupNotBusy))]
    public partial bool SetupBusy { get; set; }

    public bool SetupNotBusy => !SetupBusy;

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

        Notifications.AttachToNotifier();
    }

    /// <summary>The single persistent banner while the pipe reconnects —
    /// held by reference so it never duplicates and is removed on recovery.
    /// Non-Info notifications never auto-dissolve (NotificationCenter).</summary>
    private AppNotification? _reconnectBanner;

    private void OnConnectionChanged(EngineConnectionState state)
    {
        if (state == EngineConnectionState.Reconnecting)
        {
            if (_reconnectBanner is null)
            {
                _reconnectBanner = new AppNotification(
                    NotifySeverity.Warning,
                    Loc.Get("Notify_ReconnectingTitle"),
                    Loc.Get("Notify_ReconnectingBody"));
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
            // Unelevated, no service → the page shows the setup screen
            // (IsDisconnected); don't pretend to index.
            StatusText = Loc.Get("Status_ServiceUnregistered");
            return;
        }
        try
        {
            var volumes = await _engine.ListVolumesAsync();
            await _engine.StartIndexingAsync(volumes);
            // 起動時点の実状態を反映(pipe では接続前にサービスが索引済みのことが
            // ある)。無条件「作成中」をやめ、既Readyなら「準備完了」に。以後の
            // Scanning→Ready 遷移は OnVolumeUpdated が拾う。
            StatusText = StatusFormatter.Overall(await _engine.GetStatusAsync(), volumes);
        }
        catch (Exception ex)
        {
            FileLog.Error("engine", "startup indexing failed", ex);
            StatusText = Loc.Get("Status_IndexStartFailed");
            Notifications.Push(new AppNotification(
                NotifySeverity.Error, Loc.Get("Notify_IndexStartFailedTitle"), ex.Message));
        }
        Search.Requery(RequeryOrigin.Initial);
    }

    /// <summary>Setup screen's one-click action: register the service elevated,
    /// then (on success) wait for its pipe and relaunch — so a first-time user
    /// goes from the setup screen to a working search box in one click. The app
    /// stays unelevated; only fmf-service is elevated (per-action UAC).</summary>
    public async Task EnableSearchAsync()
    {
        if (SetupBusy)
        {
            return;
        }
        SetupBusy = true;
        SetupStatus = Loc.Get("Setup_WaitingForPermission");
        try
        {
            switch (await ServiceProvisioner.RegisterAsync())
            {
                case ServiceActionOutcome.Ok:
                    SetupStatus = Loc.Get("Setup_Connecting");
                    // On success this process relaunches and never returns here.
                    if (!await ServiceProvisioner.WaitForServiceThenRelaunchAsync())
                    {
                        SetupStatus = Loc.Get("Setup_ConnectCheckFailed");
                    }
                    break;
                case ServiceActionOutcome.Cancelled:
                    SetupStatus = string.Empty;
                    break;
                default:
                    SetupStatus = Loc.Get("Setup_Failed");
                    break;
            }
        }
        finally
        {
            SetupBusy = false;
        }
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
                Loc.Get("Notify_VolumeIndexFailedTitle", s.Label),
                Loc.Get("Notify_VolumeIndexFailedBody")));
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
        // Service-side errors are localized by status code here (the app absorbs
        // the service's English detail, which is appended for diagnostics).
        var known = e is EngineException or QuerySyntaxException or StaleResultException;
        Notifications.Push(new AppNotification(
            NotifySeverity.Error,
            known ? Loc.Get("Notify_SearchFailedTitle") : Loc.Get("Notify_SearchUnexpectedTitle"),
            known ? $"{EngineErrorText(e)}\n{e.Message}" : e.Message));
    }

    /// <summary>Localize a service/engine error by type or FMF_E_* code — the
    /// app-side absorption of the service's English-only error surface.</summary>
    private static string EngineErrorText(Exception e) => e switch
    {
        QuerySyntaxException => Loc.Get("Err_QuerySyntax"),
        StaleResultException => Loc.Get("Err_Stale"),
        EngineException { Code: var c } => c switch
        {
            2 => Loc.Get("Err_Stale"),
            3 => Loc.Get("Err_NotAdmin"),
            4 => Loc.Get("Err_Volume"),
            5 => Loc.Get("Err_QuerySyntax"),
            6 => Loc.Get("Err_Io"),
            7 => Loc.Get("Err_Locked"),
            99 => Loc.Get("Err_Panic"),
            _ => Loc.Get("Err_Generic"),
        },
        _ => Loc.Get("Err_Generic"),
    };

    /// <summary>Engine diagnostics: pull the detail text behind the POD event.</summary>
    private async Task HandleEngineErrorAsync(int severity)
    {
        await Perf.RefreshStatsAsync();
        if (severity >= 2)
        {
            var last = Perf.Stats?.RecentErrors.LastOrDefault();
            Notifications.Push(new AppNotification(
                NotifySeverity.Error,
                severity >= 3 ? Loc.Get("Notify_EnginePanicTitle") : Loc.Get("Notify_EngineErrorTitle"),
                last is null ? null : $"[{last.Area}] {Truncate(last.Message, 200)}"));
        }
    }

    private static string Truncate(string s, int max) =>
        s.Length <= max ? s : s[..max] + "…";
}
