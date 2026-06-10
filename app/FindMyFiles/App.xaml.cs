using Microsoft.UI.Xaml;
using FindMyFiles.Engine;

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
    }

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        var fake = Environment.GetCommandLineArgs()
            .Any(a => a.Equals("--fake-engine", StringComparison.OrdinalIgnoreCase));
        EngineClient = fake ? new FakeEngineClient() : new FfiEngineClient();

        DispatcherQueue = Microsoft.UI.Dispatching.DispatcherQueue.GetForCurrentThread();
        Window = new MainWindow();
        Window.Closed += (_, _) => EngineClient.Dispose();
        Window.Activate();
    }
}
