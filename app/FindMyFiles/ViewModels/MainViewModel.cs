using System.Collections.ObjectModel;
using CommunityToolkit.Mvvm.ComponentModel;
using FindMyFiles.Engine;
using FindMyFiles.Services;
using RegexScopeKind = FindMyFiles.Engine.RegexScope;

namespace FindMyFiles.ViewModels;

/// <summary>
/// Composition root of the main page: owns the UI state (search text, sort,
/// filter, status line) and the user-facing wording for failures, and wires
/// the focused components together — <see cref="SearchOrchestrator"/> (when
/// to search), <see cref="ResultsPresenter"/> (how results land),
/// <see cref="NotificationCenter"/> (InfoBar stack) and
/// <see cref="PerfPanelViewModel"/> (F12).
/// </summary>
public sealed partial class MainViewModel : ObservableObject, IDisposable
{
    private readonly IEngineClient _engine;

    /// <summary>The one place engine events cross onto the UI thread —
    /// every handler below already runs dispatched.</summary>
    private readonly EngineEventMarshaler _engineEvents;

    /// <summary>The search box text (two-way). Changes flow to the
    /// orchestrator's debounce via <c>OnSearchTextChanged</c>.</summary>
    [ObservableProperty]
    public partial string SearchText { get; set; } = string.Empty;

    /// <summary>The status-bar line — index progress, result count, or an
    /// error summary, all already localized (<see cref="StatusFormatter"/>).</summary>
    [ObservableProperty]
    public partial string StatusText { get; set; } = Loc.Get("Status_Preparing");

    /// <summary>Active sort column (name/size/mtime); changing it via
    /// <see cref="SetSort"/> requeries with <see cref="RequeryOrigin.Sort"/>.</summary>
    [ObservableProperty]
    public partial FmfSort Sort { get; set; } = FmfSort.Name;

    /// <summary>Sort direction for <see cref="Sort"/> — descending when true.</summary>
    [ObservableProperty]
    public partial bool SortDescending { get; set; }

    /// <summary>Include hidden/system files in results; flipping it is a filter
    /// change (requery with <see cref="RequeryOrigin.Filter"/>, top reset).</summary>
    [ObservableProperty]
    public partial bool IncludeHiddenSystem { get; set; }

    /// <summary>絞り込みモード (focused search, ADR-0019): the toolbar toggle.
    /// Initialized from settings in the ctor; flips push down to the
    /// orchestrator, persist, and requery as a filter change (top reset).</summary>
    [ObservableProperty]
    public partial bool FocusedSearch { get; set; }

    /// <summary>正規表現モード (ADR-0023): treat the whole query as one regex.
    /// Restored from settings in the ctor; flips persist and requery as a
    /// filter change (the same text now means something different).</summary>
    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(SearchPlaceholder))]
    public partial bool RegexMode { get; set; }

    /// <summary>Which haystack the whole-query regex matches (name/path). Only
    /// affects results while <see cref="RegexMode"/> is on, but persisted
    /// independently so it survives toggling regex off and on.</summary>
    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(SearchPlaceholder))]
    public partial RegexScopeKind RegexScope { get; set; }

    /// <summary>The search box hint — regex/scope-aware, so the box itself
    /// signals that regex mode is on (the toggle lives in the gear menu).</summary>
    public string SearchPlaceholder => RegexMode
        ? Loc.Get(RegexScope == RegexScopeKind.Path
            ? "Search_PlaceholderRegexPath"
            : "Search_PlaceholderRegexName")
        : Loc.Get("Search_Placeholder");

    // ── Disconnected setup screen (empty fake engine: no service yet) ──

    /// <summary>True when the engine is the empty fake (unelevated, no service)
    /// — the page shows the setup screen instead of a search box that can only
    /// return zero rows. Fixed for this instance's lifetime (the transport is
    /// chosen once; registering relaunches), so x:Bind OneTime is enough.</summary>
    public bool IsDisconnected => _engine is FakeEngineClient { IsEmpty: true };

    /// <summary>Inverse of <see cref="IsDisconnected"/> — true when the search
    /// UI (box + result list) should be shown instead of the setup screen.</summary>
    public bool IsReady => !IsDisconnected;

    /// <summary>Setup screen progress text ("管理者の許可を待っています…" etc.);
    /// empty hides the progress row.</summary>
    [ObservableProperty]
    public partial string SetupStatus { get; set; } = string.Empty;

    /// <summary>The setup screen's one-click action (<see cref="EnableSearchAsync"/>)
    /// is running — disables the button (<see cref="SetupNotBusy"/>) so it can't
    /// be re-triggered while a UAC prompt is up.</summary>
    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(SetupNotBusy))]
    public partial bool SetupBusy { get; set; }

    /// <summary>Inverse of <see cref="SetupBusy"/> — gates the setup button's
    /// enabled state.</summary>
    public bool SetupNotBusy => !SetupBusy;

    // ── Scope mode (ADR-0024): the no-admin path on the setup screen ──

    /// <summary>Folders the user has chosen to fold-walk in scope mode, shown on
    /// the setup screen. Seeded from settings; <see cref="StartScopeSearch"/>
    /// persists them as <see cref="AppSettings.ScopeRoots"/> and relaunches.</summary>
    public ObservableCollection<string> ScopeFolders { get; }

    /// <summary>The "start scope search" button is enabled only once at least
    /// one folder has been chosen.</summary>
    public bool CanStartScope => ScopeFolders.Count > 0;

    // CA1822 (mark static): a false positive for x:Bind targets — these surface
    // static AppPaths state to the setup screen and must be instance members of
    // the bound ViewModel for `{x:Bind ViewModel.…}` to resolve them.
#pragma warning disable CA1822
    /// <summary>True when app state lives next to the exe rather than the user
    /// profile (<see cref="AppPaths"/>) — drives the setup screen's "nothing
    /// leaves this folder" footnote. Fixed at startup, so x:Bind OneTime.</summary>
    public bool IsPortable => AppPaths.IsPortable;

    /// <summary>The portable-data-root footnote (only shown when
    /// <see cref="IsPortable"/>).</summary>
    public string DataLocationText =>
        Loc.Get("Setup_PortableLocation", AppPaths.PortableRoot ?? string.Empty);
#pragma warning restore CA1822

    /// <summary>How results land in the virtualized list (publish / refresh
    /// in place / empty) — the seam the orchestrator hands outcomes to.</summary>
    public ResultsPresenter Results { get; }

    /// <summary>Decides when and what to search (debounce, generation,
    /// requery triggers); the page forwards box edits and toggles to it.</summary>
    public SearchOrchestrator Search { get; }

    /// <summary>The InfoBar stack — failures and transient notices are pushed
    /// here.</summary>
    public NotificationCenter Notifications { get; }

    /// <summary>State behind the F12 performance panel (last trace, stats,
    /// latency history).</summary>
    public PerfPanelViewModel Perf { get; }

    private readonly AppSettings _settings;

    /// <summary>The scope-folder picker (UI boundary) — injected so the
    /// add/dedupe logic is unit-testable without showing a real dialog.</summary>
    private readonly Func<Task<string?>> _folderPicker;

    /// <summary>The unelevated relaunch action (UI/shell boundary) — injected so
    /// <see cref="StartScopeSearch"/>'s persist step is testable without exiting
    /// the process.</summary>
    private readonly Action _relaunch;

    /// <summary>Builds the focused components, restores focused-search settings,
    /// and subscribes the engine events (volume updates, errors, connection
    /// changes). Call <see cref="StartAsync"/> afterwards to begin indexing.</summary>
    /// <param name="engine">The engine client (Fake / Ffi / Pipe) this page drives.</param>
    /// <param name="dispatcher">UI dispatcher used to marshal engine callbacks
    /// and back timers.</param>
    /// <param name="settings">App settings to read/persist; loaded from disk
    /// when null.</param>
    /// <param name="folderPicker">Scope-folder picker; defaults to the real
    /// <see cref="ScopeFolderPicker.PickAsync"/> (tests inject a fake).</param>
    /// <param name="relaunch">Unelevated relaunch action; defaults to the real
    /// <see cref="ShellOps.Relaunch"/> (tests inject a no-op).</param>
    public MainViewModel(
        IEngineClient engine,
        IDispatcher dispatcher,
        AppSettings? settings = null,
        Func<Task<string?>>? folderPicker = null,
        Action? relaunch = null)
    {
        _engine = engine;
        _settings = settings ?? AppSettings.Load();
        _folderPicker = folderPicker ?? ScopeFolderPicker.PickAsync;
        _relaunch = relaunch ?? ShellOps.Relaunch;
        ScopeFolders = new ObservableCollection<string>(_settings.ScopeRoots);
        ScopeFolders.CollectionChanged += (_, _) => OnPropertyChanged(nameof(CanStartScope));
        _engineEvents = new EngineEventMarshaler(engine, dispatcher);
        Results = new ResultsPresenter(dispatcher);
        Search = new SearchOrchestrator(engine, _engineEvents, dispatcher, Results,
            () => new SearchRequest(
                SearchText,
                new SearchOptions(Sort, SortDescending, FmfCase.Smart, IncludeHiddenSystem,
                    RegexMode, RegexScope)));
        // Focused-search wiring: the lists are settings-owned; the toggle
        // state flows through OnFocusedSearchChanged (Search exists by now).
        Search.FocusedExcludePaths = _settings.FocusedExcludePaths;
        Search.FocusedExtensions = _settings.FocusedExtensions;
        FocusedSearch = _settings.FocusedSearch;
        // Regex mode/scope restore (same ctor-time no-op requery as focused).
        RegexScope = _settings.RegexScope == "path" ? RegexScopeKind.Path : RegexScopeKind.Name;
        RegexMode = _settings.RegexMode;
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

    /// <summary>Setup screen (no-admin path): open the folder picker and add the
    /// chosen folder to <see cref="ScopeFolders"/> (case-insensitive dedupe).
    /// The picker is single-select, so this adds one folder per click.</summary>
    public async Task PickScopeFoldersAsync()
    {
        var path = await _folderPicker();
        if (path is not null
            && !ScopeFolders.Any(p => string.Equals(p, path, StringComparison.OrdinalIgnoreCase)))
        {
            ScopeFolders.Add(path);
        }
    }

    /// <summary>Drop one folder from the scope list (the per-row × button).</summary>
    /// <param name="path">The folder path to remove.</param>
    public void RemoveScopeFolder(string path) => ScopeFolders.Remove(path);

    /// <summary>Persist the chosen folders as <see cref="AppSettings.ScopeRoots"/>
    /// and relaunch (unelevated): the fresh instance, with scope roots set and no
    /// running service, resolves to <c>WalkInProc</c> and folder-walks them
    /// (ADR-0024). No-op with an empty list.</summary>
    public void StartScopeSearch()
    {
        if (ScopeFolders.Count == 0)
        {
            return;
        }
        _settings.ScopeRoots = [.. ScopeFolders];
        _settings.Save();
        _relaunch();
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

    /// <summary>Regex toggle → persist + filter requery (the live query text
    /// switches between substring and whole-regex semantics). Also runs once
    /// from the ctor; the save is skipped when unchanged and the requery is a
    /// no-op on the still-empty query.</summary>
    partial void OnRegexModeChanged(bool value)
    {
        if (_settings.RegexMode != value)
        {
            _settings.RegexMode = value;
            _settings.Save();
        }
        Search.Requery(RequeryOrigin.Filter);
    }

    /// <summary>Scope radio → persist; requery only while regex mode is on
    /// (scope is inert otherwise).</summary>
    partial void OnRegexScopeChanged(RegexScopeKind value)
    {
        var s = value == RegexScopeKind.Path ? "path" : "name";
        if (_settings.RegexScope != s)
        {
            _settings.RegexScope = s;
            _settings.Save();
        }
        if (RegexMode)
        {
            Search.Requery(RequeryOrigin.Filter);
        }
    }

    /// <summary>Column-header click: re-clicking the active <see cref="Sort"/>
    /// column toggles <see cref="SortDescending"/>, a new column switches to it
    /// ascending. Either way requeries with <see cref="RequeryOrigin.Sort"/>.</summary>
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
    internal static string EngineErrorText(Exception e) => e switch
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

    /// <summary>Unsubscribe the engine-event marshaler — the one owned
    /// disposable — so its handlers stop holding this view model rooted.</summary>
    public void Dispose() => _engineEvents.Dispose();
}
