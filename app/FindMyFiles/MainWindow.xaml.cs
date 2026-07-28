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

        if (!RootFrame.Navigate(typeof(MainPage)))
        {
            throw new InvalidOperationException("initial MainPage navigation was rejected");
        }

        Activated += OnWindowActivated;
    }

    private void OnWindowActivated(
        object sender,
        Microsoft.UI.Xaml.WindowActivatedEventArgs args)
    {
        if (args.WindowActivationState
            == Microsoft.UI.Xaml.WindowActivationState.Deactivated)
        {
            return;
        }

        // Activation first gives WinUI's custom title bar global keyboard focus.
        // Defer until that focus event settles, then let the page move only
        // window-chrome focus to its state-appropriate primary action. Existing
        // content/dialog focus is preserved across ordinary reactivation.
        DispatcherQueue.TryEnqueue(() =>
        {
            if (RootFrame.Content is not MainPage page
                || AppTitleBar.XamlRoot is null)
            {
                return;
            }

            var focused = Microsoft.UI.Xaml.Input.FocusManager.GetFocusedElement(
                AppTitleBar.XamlRoot);
            if (focused is null || ReferenceEquals(focused, AppTitleBar))
            {
                page.FocusPrimaryAction();
            }
        });
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

    /// <summary>Rebuild the page graph in place (ADR-0036): re-navigate to a fresh
    /// <see cref="MainPage"/>, which reads the just-re-resolved <c>App.EngineClient</c>
    /// and rebuilds its view model. With the default <c>NavigationCacheMode</c> the
    /// old page Unloads (disposing its view model) and a new one is constructed.</summary>
    /// <returns>True when the Frame accepted the navigation.</returns>
    internal bool ReloadMainPage() => RootFrame.Navigate(typeof(MainPage));
}
