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
internal sealed partial class MainViewModel : ObservableObject, IDisposable
{
    private readonly IEngineClient _engine;
    private readonly CancellationTokenSource _lifetime = new();

    /// <summary>The one place engine events cross onto the UI thread —
    /// every handler below already runs dispatched.</summary>
    private readonly EngineEventMarshaler _engineEvents;
    private int _disposed;

    /// <summary>The search box text (two-way). Changes flow to the
    /// orchestrator's debounce via <c>OnSearchTextChanged</c>.</summary>
    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(NoResultsText))]
    public partial string SearchText { get; set; } = string.Empty;

    /// <summary>The status-bar line — index progress, result count, or an
    /// error summary, all already localized (<see cref="StatusFormatter"/>).</summary>
    [ObservableProperty]
    public partial string StatusText { get; set; } = Loc.Get("Status_Preparing");

    /// <summary>True when the current non-empty query completed with zero results
    /// — drives the "no results" empty state. Set when results land, cleared on
    /// each keystroke and on search failure so it never flashes mid-load.</summary>
    [ObservableProperty]
    public partial bool HasNoResults { get; set; }

    /// <summary>The "no results" body line, naming the searched query.</summary>
    public string NoResultsText => Loc.Get("NoResults_Body", SearchText);

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

    /// <summary>Focused search (ADR-0019): the toolbar toggle.
    /// Initialized from settings in the ctor; flips push down to the
    /// orchestrator, persist, and requery as a filter change (top reset).</summary>
    [ObservableProperty]
    public partial bool FocusedSearch { get; set; }

    /// <summary>Regex mode (ADR-0023): treat the whole query as one regex.
    /// Restored from settings in the ctor; flips persist and requery as a
    /// filter change (the same text now means something different).</summary>
    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(SearchPlaceholder))]
    [NotifyPropertyChangedFor(nameof(SearchInputPlaceholder))]
    public partial bool RegexMode { get; set; }

    /// <summary>Which haystack the whole-query regex matches (name/path). Only
    /// affects results while <see cref="RegexMode"/> is on, but persisted
    /// independently so it survives toggling regex off and on.</summary>
    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(SearchPlaceholder))]
    [NotifyPropertyChangedFor(nameof(SearchInputPlaceholder))]
    public partial RegexScopeKind RegexScope { get; set; }

    /// <summary>Tray-resident mode (ADR-0030): the gear-menu toggle. When on,
    /// closing (×) hides to the tray instead of exiting and the engine stays
    /// hot. Restored from settings in the ctor; a flip just persists — the close
    /// handler re-reads the setting from disk.</summary>
    [ObservableProperty]
    public partial bool CloseToTray { get; set; }

    /// <summary>The search box hint — regex/scope-aware, so the box itself
    /// signals that regex mode is on (the toggle lives in the gear menu).</summary>
    public string SearchPlaceholder => RegexMode
        ? Loc.Get(RegexScope == RegexScopeKind.Path
            ? "Search_PlaceholderRegexPath"
            : "Search_PlaceholderRegexName")
        : Loc.Get("Search_Placeholder");

    /// <summary>The accessible search hint. While the pipe is warming up the
    /// disabled box explains that state instead of accepting a doomed query.</summary>
    public string SearchInputPlaceholder =>
        CanSearch ? SearchPlaceholder : Loc.Get("Status_Preparing");

    // ── Disconnected setup screen (explicit unavailable engine state) ──

    /// <summary>True when no engine transport is currently available
    /// — the page shows the setup screen instead of a search box that can only
    /// return zero rows. A fatal pipe identity/protocol failure also transitions
    /// here so repair is actionable without continuing on a broken transport.</summary>
    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(IsReady))]
    public partial bool IsDisconnected { get; set; }

    /// <summary>Inverse of <see cref="IsDisconnected"/> — true when the search
    /// UI (box + result list) should be shown instead of the setup screen.</summary>
    public bool IsReady => !IsDisconnected;

    /// <summary>True only while requests can reach an engine. The search box is
    /// disabled during initial connect and reconnect, while settings and repair
    /// remain available.</summary>
    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(SearchInputPlaceholder))]
    public partial bool CanSearch { get; set; }

    /// <summary>The current index mode for the status submenu's info row
    /// (fixed NTFS drives only). Fixed for this page's lifetime (ADR-0036),
    /// so x:Bind OneTime.</summary>
    public string ModeText { get; } = Loc.Get("Status_ModePrivileged");

    /// <summary>This app's channel-aware build version line for the Settings About
    /// block (always available, from <see cref="BuildInfo"/>). Static — bound via
    /// the type in XAML; the app version is fixed for the process lifetime.</summary>
    public static string AppVersionText => Loc.Get("About_AppVersion", BuildInfo.Version);

    /// <summary>The engine/service build version, refreshed after every pipe
    /// connection and on demand by Settings. Empty until known and for in-proc
    /// clients (Ffi/Fake) where there is no separate service to ask.</summary>
    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(HasEngineVersion))]
    [NotifyPropertyChangedFor(nameof(EngineVersionText))]
    [NotifyPropertyChangedFor(nameof(HasVersionMismatch))]
    public partial string EngineVersion { get; set; } = string.Empty;

    /// <summary>Whether an engine version is known (gates the engine version row).</summary>
    public bool HasEngineVersion => EngineVersion.Length > 0;

    /// <summary>The engine version line for the About block.</summary>
    public string EngineVersionText => Loc.Get("About_EngineVersion", EngineVersion);

    /// <summary>True when app and engine come from different <c>X.Y.Z</c> bases —
    /// surfaces a warning so a stale app/service pairing is visible at a glance
    /// (both stamp the same <c>fmf-buildstamp</c> format, so the bases compare).</summary>
    public bool HasVersionMismatch =>
        HasEngineVersion && !BuildInfo.SameBase(BuildInfo.Version, EngineVersion);

    /// <summary>Best-effort fetch of the service version for About and the main
    /// version-skew warning. Stays empty for in-proc clients or if stats are
    /// unavailable, so a fetch failure never becomes a misleading warning. Call
    /// on the UI thread (it writes bound state; no ConfigureAwait(false), ADR-0036).</summary>
    /// <returns>A task that completes once the engine version has been fetched.</returns>
    public async Task RefreshVersionsAsync()
    {
        var generation = Interlocked.Increment(ref _versionRefreshGeneration);
        if (_engine.Kind != EngineClientKind.Service)
        {
            ApplyEngineVersion(string.Empty);
            return;
        }

        try
        {
            var stats = await _engine.GetStatsAsync(_lifetime.Token).ConfigureAwait(true);
            if (Volatile.Read(ref _disposed) == 0
                && generation == Volatile.Read(ref _versionRefreshGeneration))
            {
                ApplyEngineVersion(stats?.Service?.Version ?? string.Empty);
            }
        }
        catch (OperationCanceledException) when (_lifetime.IsCancellationRequested)
        {
        }
        catch (Exception ex)
        {
            if (Volatile.Read(ref _disposed) == 0
                && generation == Volatile.Read(ref _versionRefreshGeneration))
            {
                ApplyEngineVersion(string.Empty);
            }

            FileLog.Warn("engine", "engine version unavailable", ex);
        }
    }

    private void ApplyEngineVersion(string version)
    {
        EngineVersion = version;
        if (!HasVersionMismatch)
        {
            if (_versionMismatchNotification is not null)
            {
                Notifications.Remove(_versionMismatchNotification);
                _versionMismatchNotification = null;
            }

            return;
        }

        _versionMismatchNotification ??= new AppNotification(
            NotifySeverity.Warning,
            Loc.GetXaml("AboutVersionMismatch", "Title"),
            Loc.GetXaml("AboutVersionMismatch", "Message"),
            Loc.Get("VersionMismatch_RepairAction"),
            _openServiceManager,
            "VersionMismatchRepair");
        if (!Notifications.Items.Contains(_versionMismatchNotification))
        {
            Notifications.Push(_versionMismatchNotification);
        }
    }

    /// <summary>Setup screen progress text ("waiting for admin permission…" etc.);
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
    private readonly Func<bool> _saveSettings;

    /// <summary>The "make search usable" steps (register elevated → soft restart
    /// into the pipe), injected so <see cref="EnableSearchAsync"/> is testable
    /// without elevation or rebuilding the page. Defaults to
    /// <see cref="ServiceProvisioner.Real"/>.</summary>
    private readonly ServiceProvisioner _provisioner;
    private readonly Action _openServiceManager;
    private bool _restoringPersistedSetting;
    private int _versionRefreshGeneration;

    /// <summary>Builds the focused components, restores focused-search settings,
    /// and subscribes the engine events (volume updates, errors, connection
    /// changes). Call <see cref="StartAsync"/> afterwards to begin indexing.</summary>
    /// <param name="engine">The engine client (Fake / Ffi / Pipe) this page drives.</param>
    /// <param name="dispatcher">UI dispatcher used to marshal engine callbacks
    /// and back timers.</param>
    /// <param name="settings">App settings to read/persist; loaded from disk
    /// when null.</param>
    /// <param name="provisioner">The register→wait→relaunch steps behind the setup
    /// screen's one-click button; defaults to <see cref="ServiceProvisioner.Real"/>
    /// (tests inject fakes so <see cref="EnableSearchAsync"/> runs without UAC).</param>
    /// <param name="saveSettings">Persistence seam; defaults to
    /// <see cref="AppSettings.Save"/> and is injected by failure-path tests.</param>
    /// <param name="openServiceManager">Opens the service-management surface
    /// when the persistent version-skew warning's repair action is invoked.
    /// The callback keeps this ViewModel independent of WinUI view types.</param>
    public MainViewModel(
        IEngineClient engine,
        IDispatcher dispatcher,
        AppSettings? settings = null,
        ServiceProvisioner? provisioner = null,
        Func<bool>? saveSettings = null,
        Action? openServiceManager = null)
    {
        _engine = engine;
        IsDisconnected = engine.Kind == EngineClientKind.Unavailable
            || engine.Connection is EngineConnectionState.Unavailable
                or EngineConnectionState.Faulted;
        CanSearch = engine.Connection is EngineConnectionState.InProc
            or EngineConnectionState.Connected;
        _settings = settings ?? AppSettings.Load();
        _saveSettings = saveSettings ?? _settings.Save;
        _provisioner = provisioner ?? ServiceProvisioner.Real;
        _openServiceManager = openServiceManager ?? (static () => { });
        _engineEvents = new EngineEventMarshaler(engine, dispatcher);
        Results = new ResultsPresenter(dispatcher);
        Search = new SearchOrchestrator(
            engine,
            _engineEvents,
            dispatcher,
            Results,
            () => new SearchRequest(
                SearchText,
                new SearchOptions(Sort, SortDescending, FmfCase.Smart, IncludeHiddenSystem, RegexMode, RegexScope)));

        // Focused-search policy is code-owned; only the user-facing on/off
        // preference is persisted.
        FocusedSearch = _settings.FocusedSearch;

        // Regex mode/scope restore (same ctor-time no-op requery as focused).
        RegexScope = string.Equals(_settings.RegexScope, "path", StringComparison.Ordinal) ? RegexScopeKind.Path : RegexScopeKind.Name;
        RegexMode = _settings.RegexMode;
        CloseToTray = _settings.CloseToTray;
        Notifications = new NotificationCenter(dispatcher);
        Perf = new PerfPanelViewModel(engine);

        Search.TraceCaptured += Perf.RecordTrace;
        Search.SearchFailed += OnSearchFailed;
        Results.ResultsPublished += OnResultsPublished;

        _engineEvents.VolumeUpdated += OnVolumeUpdated;
        _engineEvents.EngineErrorOccurred += OnEngineErrorOccurred;
        _engineEvents.ConnectionChanged += OnConnectionChanged;

        Notifications.AttachToNotifier();
    }

    /// <summary>The single persistent banner while the pipe reconnects —
    /// held by reference so it never duplicates and is removed on recovery.
    /// Non-Info notifications never auto-dissolve (NotificationCenter).</summary>
    private AppNotification? _reconnectBanner;

    /// <summary>The non-expiring app/service version warning. Reference identity
    /// prevents repeated stats refreshes and reconnects from stacking copies.</summary>
    private AppNotification? _versionMismatchNotification;

    /// <summary>The actionable startup failure currently shown. Held by
    /// reference so a connect-driven retry or button retry replaces/removes
    /// the exact notification instead of accumulating stale errors.</summary>
    private AppNotification? _startupFailureNotification;

    /// <summary>True once <see cref="RunStartupAsync"/> has successfully run.
    /// Guards against the Loaded call and the first Connected event both running
    /// startup; cleared on a startup failure so a later connect can retry.</summary>
    private bool _started;

    private void OnEngineErrorOccurred(EngineErrorSeverity severity) =>
        HandleEngineErrorAsync(severity).Forget("engine.error");

    private void OnConnectionChanged(EngineConnectionState state)
    {
        CanSearch = state is EngineConnectionState.Connected
            or EngineConnectionState.InProc;

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

            return;
        }

        if (state is EngineConnectionState.Faulted
            or EngineConnectionState.Unavailable)
        {
            // A fatal identity/protocol mismatch stops the supervisor. Do not
            // leave the persistent "reconnecting" banner claiming recovery is
            // still in progress; the transport badge now reads disconnected
            // and the exact cause remains in app.log.
            if (_reconnectBanner is not null)
            {
                Notifications.Remove(_reconnectBanner);
                _reconnectBanner = null;
            }

            ApplyEngineVersion(string.Empty);
            IsDisconnected = true;
            return;
        }

        if (state != EngineConnectionState.Connected)
        {
            return;
        }

        IsDisconnected = false;
        if (_reconnectBanner is not null)
        {
            Notifications.Remove(_reconnectBanner);
            _reconnectBanner = null;
        }

        // ServiceInfo belongs to the newly connected pipe session. Refresh it
        // after every connect (including reconnect) so replacing a stale service
        // is reflected without requiring the user to reopen Settings.
        RefreshVersionsAsync().Forget("service-version");

        // First successful connection over a pipe — the service may have been
        // warming up when the page loaded (freshly registered, cold MFT scan). Run
        // the startup sequence now so the UI leaves "preparing" and becomes usable.
        // RunStartupAsync self-guards on _started, so a reconnect only clears the
        // banner above. Marshaled onto the UI thread by EngineEventMarshaler.
        if (!_started)
        {
            RunStartupAsync().Forget("engine.startup");
        }
    }

    /// <summary>Startup entry, called from the page's Loaded. Branches on engine
    /// readiness: an unavailable client shows setup; a pipe client that hasn't
    /// connected yet stays on "preparing" and lets <see cref="OnConnectionChanged"/>
    /// drive the real startup once it connects; an already-usable engine (FFI, or a
    /// pipe that connected before Loaded) runs it now.</summary>
    /// <returns>A task that completes once startup is kicked off (or deferred to the connect).</returns>
    public async Task StartAsync()
    {
        if (IsDisconnected)
        {
            // Unelevated, no service → the page shows the setup screen
            // (IsDisconnected); don't pretend to index.
            StatusText = Loc.Get("Status_ServiceUnregistered");
            return;
        }

        // A pipe supervisor connects asynchronously; until it has, the engine is in
        // the Connecting state and ListVolumes/StartIndexing would throw
        // EngineUnavailableException and surface a bogus "index start failed". This
        // is exactly the warm-up window a freshly registered, still-starting service
        // sits in. Hold "preparing" and let the first Connected event
        // (OnConnectionChanged) run the startup. Only a never-connected pipe reports
        // Connecting — FFI / fake / connected-pipe report InProc or Connected.
        if (_engine.Connection is EngineConnectionState.Connecting
            or EngineConnectionState.Reconnecting)
        {
            StatusText = Loc.Get("Status_Preparing");
            return;
        }

        if (_engine.Connection is EngineConnectionState.Faulted
            or EngineConnectionState.Unavailable)
        {
            IsDisconnected = true;
            StatusText = Loc.Get("Status_ServiceUnregistered");
            return;
        }

        await RefreshVersionsAsync();
        await RunStartupAsync();
    }

    /// <summary>The actual startup work once a usable engine is connected: list
    /// volumes, kick indexing, reflect status, initial requery. Self-guarding via
    /// <see cref="_started"/> so the Loaded call and a later Connected event don't
    /// double-run it; on failure it clears the flag so a subsequent Connected
    /// (e.g. after a transient warm-up error) can retry.</summary>
    /// <returns>A task that completes once startup indexing and the initial requery are kicked off.</returns>
    private async Task RunStartupAsync()
    {
        if (_started || Volatile.Read(ref _disposed) != 0)
        {
            return;
        }

        _started = true;
        try
        {
            // Stay on the dispatcher (no ConfigureAwait): the continuation sets the
            // bound StatusText and pushes notifications, so it must resume on the UI
            // thread — resuming off it throws RPC_E_WRONG_THREAD (see .editorconfig
            // CA2007/MA0004, disabled for exactly this UI-app reason).
            var ct = _lifetime.Token;
            var volumes = await _engine.ListVolumesAsync(ct);
            await _engine.StartIndexingAsync(volumes, ct);

            // Reflect the real state at startup (over a pipe the service may
            // already be indexed before we connect). Drop the unconditional
            // "preparing" and show "ready" when already Ready; later
            // Scanning→Ready transitions are picked up by OnVolumeUpdated.
            StatusText = StatusFormatter.Overall(await _engine.GetStatusAsync(ct), volumes);
            if (_startupFailureNotification is not null)
            {
                Notifications.Remove(_startupFailureNotification);
                _startupFailureNotification = null;
            }
        }
        catch (OperationCanceledException) when (_lifetime.IsCancellationRequested)
        {
            return;
        }
        catch (Exception ex)
        {
            _started = false; // let a later Connected retry the startup
            FileLog.Error("engine", "startup indexing failed", ex);
            StatusText = Loc.Get("Status_IndexStartFailed");
            if (_startupFailureNotification is not null)
            {
                Notifications.Remove(_startupFailureNotification);
            }

            _startupFailureNotification = new AppNotification(
                NotifySeverity.Error,
                Loc.Get("Notify_IndexStartFailedTitle"),
                ex.Message,
                Loc.Get("Common_Retry"),
                RetryStartup);
            Notifications.Push(_startupFailureNotification);
            return;
        }

        Search.Requery(RequeryOrigin.Initial);
    }

    private void RetryStartup()
    {
        if (_startupFailureNotification is not null)
        {
            Notifications.Remove(_startupFailureNotification);
            _startupFailureNotification = null;
        }

        RunStartupAsync().Forget("engine.startup-retry");
    }

    /// <summary>Setup screen's one-click action: register the service elevated,
    /// then (on success) re-resolve the engine in-process into the pipe — so a
    /// first-time user goes from the setup screen to a working search box in one
    /// click. The app stays unelevated; only fmf-service is elevated (per-action UAC).</summary>
    /// <returns>A task that completes when registration finishes (the soft restart
    /// rebuilds the page on success).</returns>
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
            // Stay on the dispatcher (no ConfigureAwait): every branch sets the bound
            // SetupStatus / SetupBusy, which drive the setup-screen controls
            // (button IsEnabled, progress ring, info bar) — resuming off the UI thread
            // throws RPC_E_WRONG_THREAD.
            switch (await _provisioner.RegisterAsync())
            {
                case ServiceActionOutcome.Ok:
                    if (Volatile.Read(ref _disposed) != 0)
                    {
                        return;
                    }

                    SetupStatus = Loc.Get("Setup_Connecting");

                    // Re-resolve the engine in-process forcing the pipe transport
                    // (ADR-0036): the rebuilt page's pipe supervisor waits out the
                    // just-started service's warm-up (no fixed budget), and the UI
                    // flips Setup→Ready the moment it connects.
                    _provisioner.RelaunchIntoPipe();
                    break;
                case ServiceActionOutcome.Cancelled:
                    SetupStatus = string.Empty;
                    break;
                case ServiceActionOutcome.IdentityUnavailable:
                    SetupStatus = Loc.Get("Svc_IdentityUnavailable");
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

    partial void OnSearchTextChanged(string value)
    {
        // Hide the no-results state the moment a new query is pending so it never
        // flashes mid-load; OnResultsPublished re-shows it if the query lands empty.
        HasNoResults = false;
        Search.NotifyTextChanged(value);
    }

    /// <summary>Results landed (including zero-hit publishes): show the empty
    /// state only when a non-empty query produced no rows. Authoritative signal —
    /// fires after the count is set in <see cref="ResultsPresenter.PublishAsync"/>.</summary>
    private void OnResultsPublished(ResultsPublication published)
    {
        _ = published; // the publication payload isn't needed; only that results landed
        HasNoResults = !string.IsNullOrEmpty(SearchText) && Results.ResultsSource.Count == 0;
    }

    partial void OnIncludeHiddenSystemChanged(bool value) =>
        Search.Requery(RequeryOrigin.Filter);

    /// <summary>Toggle → orchestrator + persistence + filter requery. Also
    /// runs once from the ctor (settings=true flips the default-false
    /// property): the save is skipped (no change) and the requery is a no-op
    /// on the still-empty query.</summary>
    partial void OnFocusedSearchChanged(bool value)
    {
        Search.FocusedSearch = value;
        if (!_restoringPersistedSetting && _settings.FocusedSearch != value)
        {
            var previous = _settings.FocusedSearch;
            _settings.FocusedSearch = value;
            if (!_saveSettings())
            {
                _settings.FocusedSearch = previous;
                RestorePersistedSetting(() => FocusedSearch = previous);
                ReportSettingsSaveFailure();
            }
        }

        Search.Requery(RequeryOrigin.Filter);
    }

    /// <summary>Regex toggle → persist + filter requery (the live query text
    /// switches between substring and whole-regex semantics). Also runs once
    /// from the ctor; the save is skipped when unchanged and the requery is a
    /// no-op on the still-empty query.</summary>
    partial void OnRegexModeChanged(bool value)
    {
        if (!_restoringPersistedSetting && _settings.RegexMode != value)
        {
            var previous = _settings.RegexMode;
            _settings.RegexMode = value;
            if (!_saveSettings())
            {
                _settings.RegexMode = previous;
                RestorePersistedSetting(() => RegexMode = previous);
                ReportSettingsSaveFailure();
            }
        }

        Search.Requery(RequeryOrigin.Filter);
    }

    /// <summary>Scope radio → persist; requery only while regex mode is on
    /// (scope is inert otherwise).</summary>
    partial void OnRegexScopeChanged(RegexScopeKind value)
    {
        var s = value == RegexScopeKind.Path ? "path" : "name";
        if (!_restoringPersistedSetting && _settings.RegexScope != s)
        {
            var previous = _settings.RegexScope;
            _settings.RegexScope = s;
            if (!_saveSettings())
            {
                _settings.RegexScope = previous;
                var previousScope = string.Equals(previous, "path", StringComparison.Ordinal)
                    ? RegexScopeKind.Path
                    : RegexScopeKind.Name;
                RestorePersistedSetting(() => RegexScope = previousScope);
                ReportSettingsSaveFailure();
            }
        }

        if (RegexMode)
        {
            Search.Requery(RequeryOrigin.Filter);
        }
    }

    /// <summary>Tray-resident toggle → persist only (no requery; the setting is
    /// irrelevant to search). App's close handler re-reads it from settings. Also
    /// runs once from the ctor; the save is skipped when unchanged.</summary>
    partial void OnCloseToTrayChanged(bool value)
    {
        if (!_restoringPersistedSetting && _settings.CloseToTray != value)
        {
            var previous = _settings.CloseToTray;
            _settings.CloseToTray = value;
            if (!_saveSettings())
            {
                _settings.CloseToTray = previous;
                RestorePersistedSetting(() => CloseToTray = previous);
                ReportSettingsSaveFailure();
            }
        }
    }

    private void RestorePersistedSetting(Action restore)
    {
        _restoringPersistedSetting = true;
        try
        {
            restore();
        }
        finally
        {
            _restoringPersistedSetting = false;
        }
    }

    private void ReportSettingsSaveFailure() =>
        Notifications.Push(new AppNotification(
            NotifySeverity.Error,
            Loc.Get("Settings_SaveFailedTitle"),
            Loc.Get("Settings_SaveFailedBody")));

    /// <summary>Column-header click: re-clicking the active <see cref="Sort"/>
    /// column toggles <see cref="SortDescending"/>, a new column switches to it
    /// ascending. Either way requeries with <see cref="RequeryOrigin.Sort"/>.</summary>
    /// <param name="key">The sort column the clicked header maps to.</param>
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

    /// <summary>Settings UI: set the sort direction explicitly — the settings
    /// dialog has a dedicated descending toggle, unlike the result header's
    /// click-to-flip <see cref="SetSort"/>. Requeries only on an actual change.</summary>
    /// <param name="descending">True to sort results descending.</param>
    public void SetSortDescending(bool descending)
    {
        if (SortDescending == descending)
        {
            return;
        }

        SortDescending = descending;
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
        HasNoResults = false; // an error surfaces via the InfoBar, not the empty state
        if (_engine.Connection is EngineConnectionState.Reconnecting or EngineConnectionState.Connecting)
        {
            // The connection is still settling — the reconnect banner (Reconnecting)
            // or the "preparing" startup state (Connecting) already explains it; a
            // failure here is just a request that raced the connect.
            return;
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
    /// <param name="e">The engine/service exception to map to a localized message.</param>
    /// <returns>The localized error text for the exception's type or FMF_E_* code.</returns>
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
    /// <param name="severity">The generated contract severity reported by the engine.</param>
    private async Task HandleEngineErrorAsync(EngineErrorSeverity severity)
    {
        // EngineEventMarshaler already marshaled this onto the UI thread; stay there
        // (no ConfigureAwait) — RefreshStatsAsync sets bound Perf state and the
        // continuation pushes a bound Notification.
        await Perf.RefreshStatsAsync(_lifetime.Token);
        if (Volatile.Read(ref _disposed) != 0)
        {
            return;
        }

        if (severity is EngineErrorSeverity.Error or EngineErrorSeverity.Panic)
        {
            var last = Perf.Stats?.RecentErrors.LastOrDefault();
            var title = severity == EngineErrorSeverity.Panic
                ? Loc.Get("Notify_EnginePanicTitle")
                : Loc.Get("Notify_EngineErrorTitle");
            Notifications.Push(new AppNotification(
                NotifySeverity.Error,
                title,
                last is null ? null : $"[{last.Area}] {Truncate(last.Message, 200)}"));
        }
    }

    private static string Truncate(string s, int max) =>
        s.Length <= max ? s : s[..max] + "…";

    /// <summary>Cancel every owned async flow, detach all event graphs and
    /// release the lifetime-single result handle. Idempotent.</summary>
    public void Dispose()
    {
        if (Interlocked.Exchange(ref _disposed, 1) != 0)
        {
            return;
        }

        _lifetime.Cancel();
        _engineEvents.VolumeUpdated -= OnVolumeUpdated;
        _engineEvents.EngineErrorOccurred -= OnEngineErrorOccurred;
        _engineEvents.ConnectionChanged -= OnConnectionChanged;
        Results.ResultsPublished -= OnResultsPublished;
        Search.TraceCaptured -= Perf.RecordTrace;
        Search.SearchFailed -= OnSearchFailed;
        Notifications.Dispose();
        Search.Dispose();
        Results.Dispose();
        Perf.Dispose();
        _engineEvents.Dispose();
        _lifetime.Dispose();
    }
}
