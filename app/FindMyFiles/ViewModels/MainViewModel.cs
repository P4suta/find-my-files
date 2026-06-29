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
    public partial bool RegexMode { get; set; }

    /// <summary>Which haystack the whole-query regex matches (name/path). Only
    /// affects results while <see cref="RegexMode"/> is on, but persisted
    /// independently so it survives toggling regex off and on.</summary>
    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(SearchPlaceholder))]
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

    // ── Disconnected setup screen (empty fake engine: no service yet) ──

    /// <summary>True when the engine is the empty fake (unelevated, no service)
    /// — the page shows the setup screen instead of a search box that can only
    /// return zero rows. Fixed for this page's lifetime — registering re-resolves
    /// the engine by rebuilding the page (App.SoftRestart, ADR-0036), so this
    /// fresh-page property is re-evaluated and x:Bind OneTime is enough.</summary>
    public bool IsDisconnected => _engine is FakeEngineClient { IsEmpty: true };

    /// <summary>Inverse of <see cref="IsDisconnected"/> — true when the search
    /// UI (box + result list) should be shown instead of the setup screen.</summary>
    public bool IsReady => !IsDisconnected;

    /// <summary>True once indexing in **scope mode** (ADR-0024: a user-chosen
    /// set of folders, not all drives). Gates the gear menu's "change search
    /// folders" item. Fixed for this page's lifetime (a transport change rebuilds
    /// the page, ADR-0036), so x:Bind OneTime is enough.</summary>
    public bool IsScopeMode => IsReady && _isScopeMode();

    /// <summary>True once indexing in the elevated whole-volume mode (service
    /// or in-proc). Gates the gear menu's "manage service" item — the
    /// complement of <see cref="IsScopeMode"/> while ready, both false while
    /// disconnected. Fixed for this page's lifetime (ADR-0036), so x:Bind OneTime.</summary>
    public bool IsPrivilegedMode => IsReady && !_isScopeMode();

    /// <summary>The current index mode for the status submenu's info row
    /// (selected folders vs all drives). Fixed for this page's lifetime (ADR-0036),
    /// so x:Bind OneTime.</summary>
    public string ModeText => Loc.Get(_isScopeMode() ? "Status_ModeScope" : "Status_ModePrivileged");

    /// <summary>This app's channel-aware build version line for the Settings About
    /// block (always available, from <see cref="BuildInfo"/>). Static — bound via
    /// the type in XAML; the app version is fixed for the process lifetime.</summary>
    public static string AppVersionText => Loc.Get("About_AppVersion", BuildInfo.Version);

    /// <summary>The engine/service build version, fetched on demand via
    /// <see cref="RefreshVersionsAsync"/>. Empty until known and for in-proc
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

    /// <summary>Best-effort fetch of the engine version for the About block. Stays
    /// empty for in-proc clients or if stats are unavailable — About is purely
    /// informational, so a failure must not surface as an error. Call on the UI
    /// thread (it writes a bound property; no ConfigureAwait(false), ADR-0036).</summary>
    /// <returns>A task that completes once the engine version has been fetched.</returns>
    public async Task RefreshVersionsAsync()
    {
        var stats = await _engine.GetStatsAsync().ConfigureAwait(true);
        EngineVersion = stats?.Service?.Version ?? string.Empty;
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

    // ── Scope mode (ADR-0024): the no-admin path on the setup screen ──

    /// <summary>Folders the user has chosen to fold-walk in scope mode, edited in
    /// the scope dialog. Seeded from settings; <see cref="ApplyScopeChange"/>
    /// persists them as <see cref="AppSettings.ScopeRoots"/> and relaunches.</summary>
    public ObservableCollection<string> ScopeFolders { get; }

    /// <summary>The "start scope search" button is enabled only once at least
    /// one folder has been chosen.</summary>
    public bool CanStartScope => ScopeFolders.Count > 0;

    /// <summary>Subfolders to prune from the walk (ADR-0025), shown in the scope
    /// manager dialog. Each must sit under a <see cref="ScopeFolders"/> root;
    /// seeded from settings, persisted by <see cref="ApplyScopeChange"/>.</summary>
    public ObservableCollection<string> ScopeExcludes { get; }

    /// <summary>A note naming the folders already inside a larger selected one,
    /// so the user sees the bigger set subsumes them (they merge on apply).
    /// Empty when the selection has no nesting. Recomputed on every
    /// <see cref="ScopeFolders"/> change.</summary>
    public string ScopeCoverageNote
    {
        get
        {
            var kept = ScopePaths.Normalize(ScopeFolders);
            var covered = ScopeFolders
                .Where(f => !kept.Any(k => string.Equals(k, f, StringComparison.OrdinalIgnoreCase)))
                .ToList();
            return covered.Count == 0
                ? string.Empty
                : Loc.Get("Scope_CoverageNote", string.Join(", ", covered));
        }
    }

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

    /// <summary>The in-process soft-restart action (UI/shell boundary) — injected
    /// so <see cref="ApplyScopeChange"/>'s persist step is testable without
    /// rebuilding the page. Defaults to <see cref="App.SoftRestart"/>.</summary>
    private readonly Action _relaunch;

    /// <summary>Reports whether the live engine is a scope-mode walk (ADR-0024)
    /// — injected so the mode-driven UI (<see cref="IsScopeMode"/> /
    /// <see cref="IsPrivilegedMode"/> / <see cref="ModeText"/>) is testable with
    /// a stub engine. Defaults to inspecting the real <see cref="FfiEngineClient"/>.</summary>
    private readonly Func<bool> _isScopeMode;

    /// <summary>The "make search usable" steps (register elevated → soft restart
    /// into the pipe), injected so <see cref="EnableSearchAsync"/> is testable
    /// without elevation or rebuilding the page. Defaults to
    /// <see cref="ServiceProvisioner.Real"/>.</summary>
    private readonly ServiceProvisioner _provisioner;

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
    /// <param name="relaunch">In-process soft-restart action; defaults to the real
    /// <see cref="App.SoftRestart"/> (tests inject a no-op).</param>
    /// <param name="isScopeMode">Reports whether the engine is a scope-mode walk;
    /// defaults to inspecting the real <see cref="FfiEngineClient"/> (tests inject
    /// a constant to drive the mode-dependent UI).</param>
    /// <param name="provisioner">The register→wait→relaunch steps behind the setup
    /// screen's one-click button; defaults to <see cref="ServiceProvisioner.Real"/>
    /// (tests inject fakes so <see cref="EnableSearchAsync"/> runs without UAC).</param>
    public MainViewModel(
        IEngineClient engine,
        IDispatcher dispatcher,
        AppSettings? settings = null,
        Func<Task<string?>>? folderPicker = null,
        Action? relaunch = null,
        Func<bool>? isScopeMode = null,
        ServiceProvisioner? provisioner = null)
    {
        _engine = engine;
        _settings = settings ?? AppSettings.Load();
        _folderPicker = folderPicker ?? ScopeFolderPicker.PickAsync;
        _relaunch = relaunch ?? App.SoftRestart;
        _isScopeMode = isScopeMode ?? (() => _engine is FfiEngineClient { IsScopeMode: true });
        _provisioner = provisioner ?? ServiceProvisioner.Real;
        ScopeFolders = new ObservableCollection<string>(_settings.ScopeRoots);
        ScopeExcludes = new ObservableCollection<string>(_settings.ScopeExcludes);
        ScopeFolders.CollectionChanged += (_, _) =>
        {
            OnPropertyChanged(nameof(CanStartScope));
            OnPropertyChanged(nameof(ScopeCoverageNote));
        };
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

        // Focused-search wiring: the lists are settings-owned; the toggle
        // state flows through OnFocusedSearchChanged (Search exists by now).
        Search.FocusedExcludePaths = _settings.FocusedExcludePaths;
        Search.FocusedExtensions = _settings.FocusedExtensions;
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
        _engineEvents.EngineErrorOccurred += severity =>
            HandleEngineErrorAsync(severity).Forget("engine.error");
        _engineEvents.ConnectionChanged += OnConnectionChanged;

        Notifications.AttachToNotifier();
    }

    /// <summary>The single persistent banner while the pipe reconnects —
    /// held by reference so it never duplicates and is removed on recovery.
    /// Non-Info notifications never auto-dissolve (NotificationCenter).</summary>
    private AppNotification? _reconnectBanner;

    /// <summary>True once <see cref="RunStartupAsync"/> has successfully run.
    /// Guards against the Loaded call and the first Connected event both running
    /// startup; cleared on a startup failure so a later connect can retry.</summary>
    private bool _started;

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

            return;
        }

        if (state != EngineConnectionState.Connected)
        {
            return;
        }

        if (_reconnectBanner is not null)
        {
            Notifications.Remove(_reconnectBanner);
            _reconnectBanner = null;
        }

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
    /// readiness: the empty fake shows the setup screen; a pipe client that hasn't
    /// connected yet stays on "preparing" and lets <see cref="OnConnectionChanged"/>
    /// drive the real startup once it connects; an already-usable engine (FFI, or a
    /// pipe that connected before Loaded) runs it now.</summary>
    /// <returns>A task that completes once startup is kicked off (or deferred to the connect).</returns>
    public async Task StartAsync()
    {
        if (_engine is FakeEngineClient { IsEmpty: true })
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
        if (_engine.Connection == EngineConnectionState.Connecting)
        {
            StatusText = Loc.Get("Status_Preparing");
            return;
        }

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
        if (_started)
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
            var volumes = await _engine.ListVolumesAsync();
            await _engine.StartIndexingAsync(volumes);

            // Reflect the real state at startup (over a pipe the service may
            // already be indexed before we connect). Drop the unconditional
            // "preparing" and show "ready" when already Ready; later
            // Scanning→Ready transitions are picked up by OnVolumeUpdated.
            StatusText = StatusFormatter.Overall(await _engine.GetStatusAsync(), volumes);
        }
        catch (Exception ex)
        {
            _started = false; // let a later Connected retry the startup
            FileLog.Error("engine", "startup indexing failed", ex);
            StatusText = Loc.Get("Status_IndexStartFailed");
            Notifications.Push(new AppNotification(
                NotifySeverity.Error, Loc.Get("Notify_IndexStartFailedTitle"), ex.Message));
        }

        Search.Requery(RequeryOrigin.Initial);
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
    /// <returns>A task that completes once the picked folder (if any) has been added.</returns>
    public async Task PickScopeFoldersAsync()
    {
        // No ConfigureAwait(false): the picker is a genuinely async OS dialog, and the
        // continuation mutates the bound ScopeFolders — whose CollectionChanged drives
        // the start button's IsEnabled via x:Bind. Resuming off the dispatcher updates a
        // control from a pool thread → COMException 0x8001010E (RPC_E_WRONG_THREAD), which
        // .Forget swallows, so the user silently can't proceed. The UI app resumes on the
        // dispatcher by convention (.editorconfig disables CA2007/MA0004 for this reason).
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

    /// <summary>Manager dialog (scope mode): pick a subfolder to prune from the
    /// walk (ADR-0025). Rejected with a notice when it is not inside one of the
    /// chosen <see cref="ScopeFolders"/> roots (an exclude outside the indexed
    /// set prunes nothing). Case-insensitive dedupe.</summary>
    /// <returns>A task that completes once the picked folder (if valid) is added.</returns>
    public async Task PickScopeExcludeAsync()
    {
        // No ConfigureAwait(false): the continuation mutates bound collections,
        // so it must resume on the UI thread (see PickScopeFoldersAsync).
        var path = await _folderPicker();
        if (path is null)
        {
            return;
        }

        if (!ScopePaths.IsUnderAnyRoot(path, ScopeFolders))
        {
            Notifications.Push(new AppNotification(
                NotifySeverity.Warning, Loc.Get("Scope_ExcludeNotUnderRoot"), path));
            return;
        }

        if (!ScopeExcludes.Any(p => string.Equals(p, path, StringComparison.OrdinalIgnoreCase)))
        {
            ScopeExcludes.Add(path);
        }
    }

    /// <summary>Drop one excluded subfolder (the per-row × button).</summary>
    /// <param name="path">The exclude path to remove.</param>
    public void RemoveScopeExclude(string path) => ScopeExcludes.Remove(path);

    /// <summary>Apply the current <see cref="ScopeFolders"/> as the scope: drop
    /// roots nested under another (<see cref="ScopePaths.Normalize"/>), and if the
    /// set actually changed, persist it as <see cref="AppSettings.ScopeRoots"/>
    /// and relaunch (unelevated) into a fresh <c>WalkInProc</c> that folder-walks
    /// the new set (ADR-0024). The engine has no live root-swap
    /// (<c>index_start_scope</c> no-ops on an existing scope slot), so a relaunch
    /// is the only way to re-walk. No-op when empty or unchanged, so re-opening
    /// the manager and closing it without edits never restarts.</summary>
    public void ApplyScopeChange()
    {
        var roots = ScopePaths.Normalize(ScopeFolders);
        if (roots.Count == 0)
        {
            return;
        }

        // Keep only excludes still inside a (normalized) root — a removed root
        // makes its excludes moot; the engine ignores non-matching ones anyway.
        var excludes = ScopeExcludes
            .Where(e => ScopePaths.IsUnderAnyRoot(e, roots))
            .ToList();

        if (SameSet(roots, _settings.ScopeRoots) && SameSet(excludes, _settings.ScopeExcludes))
        {
            return;
        }

        _settings.ScopeRoots = [.. roots];
        _settings.ScopeExcludes = [.. excludes];
        _settings.Save();
        _relaunch();
    }

    /// <summary>Order- and case-insensitive set equality, so a reorder or
    /// case-only edit counts as "unchanged" and skips the relaunch.</summary>
    private static bool SameSet(List<string> a, string[] b) =>
        a.Count == b.Length
        && a.OrderBy(p => p, StringComparer.OrdinalIgnoreCase)
            .SequenceEqual(
                b.OrderBy(p => p, StringComparer.OrdinalIgnoreCase),
                StringComparer.OrdinalIgnoreCase);

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

    /// <summary>Tray-resident toggle → persist only (no requery; the setting is
    /// irrelevant to search). App's close handler re-reads it from settings. Also
    /// runs once from the ctor; the save is skipped when unchanged.</summary>
    partial void OnCloseToTrayChanged(bool value)
    {
        if (_settings.CloseToTray != value)
        {
            _settings.CloseToTray = value;
            _settings.Save();
        }
    }

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
    /// <param name="severity">The reported error severity (≥2 surfaces a notification, ≥3 is a panic).</param>
    private async Task HandleEngineErrorAsync(int severity)
    {
        // EngineEventMarshaler already marshaled this onto the UI thread; stay there
        // (no ConfigureAwait) — RefreshStatsAsync sets bound Perf state and the
        // continuation pushes a bound Notification.
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
