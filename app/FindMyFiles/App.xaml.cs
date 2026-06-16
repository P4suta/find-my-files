using FindMyFiles.Engine;
using FindMyFiles.Services;
using Microsoft.UI.Xaml;

namespace FindMyFiles;

/// <summary>
/// アプリのエントリポイント兼プロセス全体の合流点。`OnLaunched` でエンジン境界
/// (<see cref="EngineClient"/>)を解決し、唯一の <see cref="MainWindow"/> を立てる。
/// 致命的初期化失敗時は `FakeEngineClient` へフォールバックして「黙って落ちる」を避ける。
/// </summary>
// View/startup shell: imperative UI wiring + composition root, not unit-tested (ADR-0022).
[System.Diagnostics.CodeAnalysis.ExcludeFromCodeCoverage]
public partial class App : Application
{
    /// <summary>唯一のトップレベルウィンドウ。`OnLaunched` で生成され、`WinRT.Interop`
    /// 経由の HWND 取得(<see cref="WindowHandle"/>)の起点になる。</summary>
    public static Window Window { get; private set; } = null!;

    /// <summary>UI スレッドの <c>DispatcherQueue</c>(`OnLaunched` 内でキャッシュ)。
    /// バックグラウンドからの UI マーシャリングはこれ経由の <c>TryEnqueue</c> で行う
    /// (UI固定則: UIスレッドでキャッシュしてから使う)。</summary>
    public static Microsoft.UI.Dispatching.DispatcherQueue DispatcherQueue { get; private set; } = null!;

    /// <summary>
    /// The engine boundary. `--fake-engine` swaps in deterministic data so UI
    /// tests and unelevated development never touch real volumes.
    /// </summary>
    public static IEngineClient EngineClient { get; private set; } = null!;

    /// <summary>メインウィンドウの Win32 HWND。ファイルピッカー等、ウィンドウを
    /// 親に取る WinRT API の初期化に渡す(unpackaged WinUI 3 の作法)。</summary>
    public static nint WindowHandle =>
        WinRT.Interop.WindowNative.GetWindowHandle(Window);

    /// <summary>言語上書きの適用 → `InitializeComponent` → <c>ExceptionPolicy.Install</c>
    /// の順で初期化する。言語適用は最初の XAML ロードで `x:Uid`/`ResourceLoader` が
    /// 正しい言語に解決されるよう `InitializeComponent` より前に行う必要がある。</summary>
    public App()
    {
        // Must run before InitializeComponent so x:Uid / ResourceLoader resolve
        // to the chosen language from the first XAML load.
        ApplyLanguageOverride();

        InitializeComponent();

        // 「落ちない・固まらない・黙らない」: suppression rules, crash markers
        // and log routing are documented in one place — ExceptionPolicy.
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
            // (ARCHITECTURE.md FMF_E_LOCKED の指針). The factory's QueryState
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
