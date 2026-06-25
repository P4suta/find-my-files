using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;

namespace FindMyFiles;

/// <summary>The application window: hosts the root Frame; app UI lives in
/// MainPage.</summary>
// View shell: window chrome + title bar, not unit-tested (ADR-0022).
[System.Diagnostics.CodeAnalysis.ExcludeFromCodeCoverage]
public sealed partial class MainWindow : Window
{
    /// <summary>Extends the title bar into the content, sets the window icon,
    /// subscribes the tray-resident close handler, and navigates the root Frame
    /// to <see cref="MainPage"/>.</summary>
    public MainWindow()
    {
        InitializeComponent();

        ExtendsContentIntoTitleBar = true;
        SetTitleBar(AppTitleBar);

        AppWindow.SetIcon("Assets/AppIcon.ico");
        AppWindow.Closing += OnClosing;

        RootFrame.Navigate(typeof(MainPage));
    }

    // Tray-resident mode (ADR-0030): when enabled, a close (×) hides to the tray
    // instead of exiting. The decision and the real-exit override live in App.
    private void OnClosing(AppWindow sender, AppWindowClosingEventArgs args)
    {
        if (App.HandleMainWindowClosing())
        {
            args.Cancel = true;
        }
    }
}
