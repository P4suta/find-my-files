using Microsoft.UI.Xaml;
using FindMyFiles.Engine;
using FindMyFiles.Services;

namespace FindMyFiles;

public partial class App : Application
{
    public static Window Window { get; private set; } = null!;

    public static Microsoft.UI.Dispatching.DispatcherQueue DispatcherQueue { get; private set; } = null!;

    /// <summary>
    /// The engine boundary. `--fake-engine` swaps in deterministic data so UI
    /// tests and unelevated development never touch real volumes.
    /// </summary>
    public static IEngineClient EngineClient { get; private set; } = null!;

    /// <summary>The daily user's SID, forwarded from the unelevated instance
    /// via --setup-owner so the in-app service registration authorizes it on
    /// the pipe even under OTS elevation. Null on a normal launch.</summary>
    public static string? SetupOwnerSid { get; private set; }

    public static nint WindowHandle =>
        WinRT.Interop.WindowNative.GetWindowHandle(Window);

    public App()
    {
        InitializeComponent();

        // 「落ちない・固まらない・黙らない」: suppression rules, crash markers
        // and log routing are documented in one place — ExceptionPolicy.
        ExceptionPolicy.Install(this);
    }

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        FileLog.Info("app", $"launch v{typeof(App).Assembly.GetName().Version} "
            + $"os={Environment.OSVersion.VersionString}");

        var cmdLine = Environment.GetCommandLineArgs();
        SetupOwnerSid = ParseSetupOwner(cmdLine);

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
                "検索サービスが稼働中のため in-proc エンジンを使えません",
                "通常起動でサービスに接続できます。in-proc を使う場合は先に"
                + "サービスを停止してください(`just service-stop`)。");
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
                "検索エンジンの初期化に失敗しました(フォールバック動作中)",
                $"{ex.Message}\nfmf-service が未起動か、インデックスを他のエンジンが"
                + "ロックしている可能性があります(詳細は engine.log)。");
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

    /// <summary>Extract and validate --setup-owner=&lt;sid&gt; (forwarded by
    /// the unelevated instance on elevation) — see
    /// <see cref="ServiceSetup.CurrentUserSid"/> and
    /// <see cref="SetupOwnerSid"/>.</summary>
    private static string? ParseSetupOwner(string[] args)
    {
        const string prefix = "--setup-owner=";
        var raw = args
            .FirstOrDefault(a => a.StartsWith(prefix, StringComparison.OrdinalIgnoreCase))
            ?[prefix.Length..];
        return ServiceSetup.IsValidSid(raw) ? raw : null;
    }
}
