using FindMyFiles.Engine;
using FindMyFiles.Services;
using FindMyFiles.ViewModels;
using FindMyFiles.Views;
using Microsoft.UI.Xaml;

namespace FindMyFiles;

/// <summary>
/// Application entry point and process-wide composition root. `OnLaunched`
/// resolves the engine boundary (<see cref="EngineClient"/>) and stands up the
/// single <see cref="MainWindow"/>. On fatal init failure it falls back to
/// `FakeEngineClient` to avoid crashing silently.
/// </summary>
// View/startup shell: imperative UI wiring + composition root, not unit-tested (ADR-0022).
[System.Diagnostics.CodeAnalysis.ExcludeFromCodeCoverage]
public partial class App : Application
{
    /// <summary>The single top-level window. Created in `OnLaunched`; the origin
    /// for the HWND lookup via `WinRT.Interop` (<see cref="WindowHandle"/>).</summary>
    public static Window Window { get; private set; } = null!;

    /// <summary>The UI thread's <c>DispatcherQueue</c> (cached in `OnLaunched`).
    /// Marshal UI work from background threads via <c>TryEnqueue</c> on this
    /// (UI rule: cache it on the UI thread before use).</summary>
    public static Microsoft.UI.Dispatching.DispatcherQueue DispatcherQueue { get; private set; } = null!;

    /// <summary>
    /// The engine boundary. `--fake-engine` swaps in deterministic data so UI
    /// tests and unelevated development never touch real volumes.
    /// </summary>
    public static IEngineClient EngineClient { get; private set; } = null!;

    /// <summary>The main window's Win32 HWND. Passed to init of WinRT APIs that
    /// take a parent window (file pickers, etc.) — the unpackaged WinUI 3 way.</summary>
    public static nint WindowHandle =>
        WinRT.Interop.WindowNative.GetWindowHandle(Window);

    /// <summary>The non-modal diagnostics window, or <c>null</c> when closed. A
    /// single instance toggled by <see cref="ToggleDiagnostics"/>; the main
    /// window's `Closed` handler closes it first so no orphan top-level window
    /// survives shutdown.</summary>
    private static DiagnosticsWindow? _diagWindow;

    /// <summary>The system-tray icon (ADR-0030), or <c>null</c> when tray init
    /// failed or before the window is up. Held for the process lifetime.</summary>
    private static TrayIcon? _tray;

    /// <summary>Set when the user really quits (tray "Exit") so the main window's
    /// Closing handler lets the close proceed instead of hiding to tray.</summary>
    private static bool _explicitExit;

    /// <summary>The in-process soft-restart orchestrator (ADR-0036), wired in
    /// <see cref="OnLaunched"/> once the window exists. Null only before launch.</summary>
    private static AppReload? _reload;

    /// <summary>Open or close the diagnostics window, sharing the supplied
    /// <see cref="PerfPanelViewModel"/> (the single `MainViewModel.Perf`
    /// instance). When already open, closing it is enough; the `Closed` handler
    /// clears the reference and stops polling via <c>IsOpen</c>. When opening,
    /// setting <c>IsOpen</c> to <see langword="true"/> starts the 1 Hz timer.
    /// UI-thread serialized, so the open/close toggle has no race.</summary>
    /// <param name="perf">The shared performance view model to display.</param>
    public static void ToggleDiagnostics(PerfPanelViewModel perf)
    {
        if (_diagWindow is not null)
        {
            _diagWindow.Close();
            return;
        }

        var win = new DiagnosticsWindow(perf);
        win.Closed += (_, _) =>
        {
            _diagWindow = null;
            perf.IsOpen = false;
        };
        _diagWindow = win;
        win.Activate();
        perf.IsOpen = true;          // panel is subscribed → SyncTimer starts polling
    }

    /// <summary>Initialize in order: apply language override → `InitializeComponent`
    /// → <c>ExceptionPolicy.Install</c>. The language override must run before
    /// `InitializeComponent` so `x:Uid`/`ResourceLoader` resolve to the correct
    /// language on the first XAML load.</summary>
    public App()
    {
        // Stand the logger up first: ApplyLanguageOverride and ExceptionPolicy
        // below both log, and every FileLog call routes through it (ADR-0037).
        LogSetup.Init();

        // Must run before InitializeComponent so x:Uid / ResourceLoader resolve
        // to the chosen language from the first XAML load.
        ApplyLanguageOverride();

        InitializeComponent();

        // "don't crash / don't hang / don't go silent": suppression rules, crash
        // markers and log routing are documented in one place — ExceptionPolicy.
        ExceptionPolicy.Install(this);
    }

    /// <summary>Apply the persisted UI language. "auto" (the default) follows
    /// the OS display language; anything else overrides it. Unpackaged apps use
    /// the WinAppSDK ApplicationLanguages (the UWP one needs package identity).</summary>
    private static void ApplyLanguageOverride()
    {
        try
        {
            var lang = AppSettings.Load().Language;
            if (!string.IsNullOrEmpty(lang) && !string.Equals(lang, "auto", StringComparison.Ordinal))
            {
                Microsoft.Windows.Globalization.ApplicationLanguages.PrimaryLanguageOverride = lang;
            }
        }
        catch (Exception ex)
        {
            FileLog.Warn("i18n", $"language override failed: {ex.Message}");
        }
    }

    /// <inheritdoc/>
    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        FileLog.Info("app", $"launch v{BuildInfo.Version} os={Environment.OSVersion.VersionString}");

        var cmdLine = Environment.GetCommandLineArgs();
        EngineClient = ResolveEngineOrFallback(cmdLine);

        ExceptionPolicy.ReportPreviousCrash();

        DispatcherQueue = Microsoft.UI.Dispatching.DispatcherQueue.GetForCurrentThread();
        Window = new MainWindow();
        Window.Closed += (_, _) =>
        {
            // Close the diagnostics window first so no orphan top-level window
            // survives shutdown (mandatory correction 1).
            _diagWindow?.Close();
            _tray?.Dispose();
            try
            {
                EngineClient.Dispose();
            }
            catch (Exception ex)
            {
                FileLog.Warn("app", "engine dispose failed", ex);
            }

            // Flush the last lines to app.log before the process exits.
            LogSetup.Shutdown();
        };
        Window.Activate();

        // Tray-resident mode (ADR-0030): the icon keeps the process (and its hot
        // engine connection) alive after a close-to-tray. Best-effort — the app
        // is fully usable without it.
        _tray = CreateTrayIcon();

        // The in-process soft restart (ADR-0036): re-resolve the engine and rebuild
        // the page without spawning a process. Wired here, after the window exists,
        // because the rebuild re-navigates its Frame and the teardown closes the
        // diagnostics window.
        _reload = new AppReload(
            resolve: ResolveEngineOrFallback,
            getEngine: () => EngineClient,
            setEngine: engine => EngineClient = engine,
            renavigate: () => ((MainWindow)Window).ReloadMainPage(),
            closeDiagnostics: () => _diagWindow?.Close());
    }

    /// <summary>Resolve the engine transport, degrading to the empty/locked fake
    /// (with the matching notification) on failure. Shared by startup
    /// (<see cref="OnLaunched"/>) and the in-process soft restart so the
    /// fall-back wording can never drift between the two paths.</summary>
    /// <param name="args">Engine-selection args (process command line, or a
    /// soft-restart override such as <c>--engine=pipe</c>).</param>
    /// <returns>The resolved engine, or a fake when initialization failed.</returns>
    private static IEngineClient ResolveEngineOrFallback(string[] args)
    {
        try
        {
            return EngineClientFactory.Resolve(args);
        }
        catch (EngineException ex) when (ex.Code == EngineContract.Status.Locked)
        {
            // The service is up and holds the writer lock — in-proc cannot
            // start here. Say exactly that instead of the generic failure
            // (ARCHITECTURE.md FMF_E_LOCKED guidance). The factory's QueryState
            // guard means we rarely reach this — it's the backstop.
            FileLog.Error("app", "engine init: index locked by the running service", ex);
            Notifier.Post(
                NotifySeverity.Error,
                Loc.Get("App_LockedTitle"),
                Loc.Get("App_LockedBody"));
            return new FakeEngineClient();
        }
        catch (Exception ex)
        {
            // Engine refused to start (DLL missing, service down, index dir
            // locked by another engine…) — run the UI on the fake engine so
            // the user sees an explanation instead of an instant crash.
            FileLog.Error("app", "engine initialization failed; falling back to fake", ex);
            Notifier.Post(
                NotifySeverity.Error,
                Loc.Get("App_EngineInitFailedTitle"),
                Loc.Get("App_EngineInitFailedBody", ex.Message));
            return new FakeEngineClient();
        }
    }

    /// <summary>In-process soft restart (ADR-0036): re-resolve the engine from the
    /// current process command line and rebuild the page. Used after a scope change
    /// or service uninstall, and the manual "restart app" button — anything that
    /// only needs the once-at-startup transport choice re-made. Marshals onto the UI
    /// thread (callers may resume off it after an elevated step).</summary>
    public static void SoftRestart() =>
        OnUiThread(() => _reload?.Run(Environment.GetCommandLineArgs()));

    /// <summary>In-process soft restart forcing the pipe transport, used right
    /// after a successful in-app service registration: the rebuilt page binds the
    /// retrying pipe client directly (its supervisor waits out the freshly-started
    /// service's warm-up) instead of re-running <c>auto</c> detection, whose single
    /// short probe can miss the not-yet-answering service and fall back to the empty
    /// engine. Replaces the old process relaunch, which single-instancing defeated
    /// (ADR-0036).</summary>
    public static void SoftRestartIntoPipe() =>
        OnUiThread(() => _reload?.Run(WithPipeEngine(Environment.GetCommandLineArgs())));

    /// <summary>Force <c>--engine=pipe</c> on a copy of <paramref name="args"/>,
    /// dropping any existing <c>--engine=</c> token first (the factory's
    /// <c>OptionValue</c> takes the first match, so a leftover <c>--engine=empty</c>
    /// test seam would otherwise win).</summary>
    /// <param name="args">The base args to copy and override.</param>
    /// <returns>A new args array selecting the pipe transport.</returns>
    private static string[] WithPipeEngine(string[] args) =>
    [
        .. args.Where(a => !a.StartsWith("--engine=", StringComparison.OrdinalIgnoreCase)),
        "--engine=pipe",
    ];

    /// <summary>Run <paramref name="action"/> on the cached UI dispatcher — inline
    /// when already on it, marshaled otherwise (a soft restart touches the Frame and
    /// the diagnostics window, which are UI-thread-affine).</summary>
    /// <param name="action">The UI-thread work.</param>
    private static void OnUiThread(Action action)
    {
        if (DispatcherQueue.HasThreadAccess)
        {
            action();
        }
        else
        {
            DispatcherQueue.TryEnqueue(() => action());
        }
    }

    /// <summary>Creates the tray icon, or <c>null</c> when it cannot be
    /// initialized — the app stays fully usable without it (ADR-0030).</summary>
    private static TrayIcon? CreateTrayIcon()
    {
        try
        {
            return new TrayIcon(WindowHandle, Loc.Get("Tray_Tooltip"), ShowFromTray, ExitApplication);
        }
        catch (Exception ex)
        {
            FileLog.Warn("tray", $"tray icon init failed: {ex.Message}");
            return null;
        }
    }

    /// <summary>Decides whether the main window's close (×) should hide to the
    /// tray rather than exit (ADR-0030). Called from MainWindow's
    /// <c>AppWindow.Closing</c>.</summary>
    /// <returns>True to cancel the close (now hidden to tray); false to exit.</returns>
    internal static bool HandleMainWindowClosing()
    {
        if (WindowLifecycle.ShouldHideToTray(AppSettings.Load().CloseToTray, _explicitExit))
        {
            Window.AppWindow.Hide();
            return true;
        }

        return false;
    }

    /// <summary>Restores the window from the tray (left-click / "Open"). Runs on
    /// the UI thread (the subclass proc's thread); a click is user input, so
    /// Activate brings it to the foreground.</summary>
    internal static void ShowFromTray()
    {
        Window.AppWindow.Show();
        Window.Activate();
    }

    /// <summary>Really exits (tray "Exit"): marks the exit explicit so Closing
    /// lets the window close, runs the normal teardown, then exits the process —
    /// Hide had kept it alive past the last window.</summary>
    internal static void ExitApplication()
    {
        _explicitExit = true;
        _tray?.Dispose(); // remove the icon now so no ghost survives if Close runs async
        Window.Close();
        Current.Exit();
    }
}
