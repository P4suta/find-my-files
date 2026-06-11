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

        try
        {
            EngineClient = EngineClientFactory.Resolve(Environment.GetCommandLineArgs());
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
}
