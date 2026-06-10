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

    private int _unhandledStorm;

    public App()
    {
        InitializeComponent();

        // 「落ちない・固まらない・黙らない」: every escape hatch logs, and
        // recoverable ones surface in the InfoBar instead of killing the app.
        UnhandledException += (_, e) =>
        {
            FileLog.Error("xaml", "unhandled exception", e.Exception);
            if (System.Threading.Interlocked.Increment(ref _unhandledStorm) <= 3)
            {
                e.Handled = true;
                Notifier.Post(
                    NotifySeverity.Error,
                    "予期しないエラーが発生しました",
                    e.Exception?.Message);
            }
            else
            {
                // Exception storm — record and let the process die honestly.
                FileLog.WriteCrashMarker(e.Exception?.ToString() ?? "exception storm");
            }
        };
        AppDomain.CurrentDomain.UnhandledException += (_, e) =>
        {
            var ex = e.ExceptionObject as Exception;
            FileLog.Error("appdomain", "fatal unhandled exception", ex);
            FileLog.WriteCrashMarker(ex?.ToString() ?? "unknown fatal exception");
        };
        TaskScheduler.UnobservedTaskException += (_, e) =>
        {
            FileLog.Error("task", "unobserved task exception", e.Exception);
            e.SetObserved();
            Notifier.Post(
                NotifySeverity.Error,
                "バックグラウンド処理でエラーが発生しました",
                e.Exception.InnerException?.Message ?? e.Exception.Message);
        };
    }

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        FileLog.Info("app", $"launch v{typeof(App).Assembly.GetName().Version} "
            + $"os={Environment.OSVersion.VersionString}");

        var fake = Environment.GetCommandLineArgs()
            .Any(a => a.Equals("--fake-engine", StringComparison.OrdinalIgnoreCase));
        try
        {
            EngineClient = fake ? new FakeEngineClient() : new FfiEngineClient();
        }
        catch (Exception ex)
        {
            // Engine refused to start (DLL missing, config issue…) — run the
            // UI on the fake engine so the user sees an explanation instead
            // of an instant crash.
            FileLog.Error("app", "engine initialization failed; falling back to fake", ex);
            Notifier.Post(
                NotifySeverity.Error,
                "検索エンジンの初期化に失敗しました(フォールバック動作中)",
                ex.Message);
            EngineClient = new FakeEngineClient();
        }

        if (FileLog.TakeCrashMarker() is { } marker)
        {
            FileLog.Warn("app", "previous run crashed");
            Notifier.Post(
                NotifySeverity.Warning,
                "前回、アプリが異常終了しました",
                $"詳細: {FileLog.LogPath}\n{marker.Split('\n').FirstOrDefault()}");
        }

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
