using FindMyFiles.Engine;
using FindMyFiles.Services;
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
    }
}
