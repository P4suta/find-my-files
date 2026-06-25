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
        FileLog.Info("app", $"launch v{typeof(App).Assembly.GetName().Version} os={Environment.OSVersion.VersionString}");

        var cmdLine = Environment.GetCommandLineArgs();

        try
        {
            EngineClient = EngineClientFactory.Resolve(cmdLine);
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
            EngineClient = new FakeEngineClient();
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
            EngineClient = new FakeEngineClient();
        }

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
        };
        Window.Activate();

        // Tray-resident mode (ADR-0030): the icon keeps the process (and its hot
        // engine connection) alive after a close-to-tray. Best-effort — the app
        // is fully usable without it.
        _tray = CreateTrayIcon();
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
