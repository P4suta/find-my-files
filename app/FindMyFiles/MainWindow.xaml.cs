using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;

namespace FindMyFiles;

/// <summary>The application window: hosts the root Frame; app UI lives in MainPage.</summary>
[System.Diagnostics.CodeAnalysis.ExcludeFromCodeCoverage]
public sealed partial class MainWindow : Window
{
    /// <summary>Extends the title bar into the content, sets the window icon,
    /// subscribes the tray-resident close handler, and navigates the root Frame to <see cref="MainPage"/>.</summary>
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

    private void OnClosing(AppWindow sender, AppWindowClosingEventArgs args)
    {
        if (App.HandleMainWindowClosing())
        {
            args.Cancel = true;
        }
    }

    /// <summary>Rebuild the page graph in place (ADR-0036): re-navigate to a fresh <see cref="MainPage"/>, which reads the just-re-resolved <c>App.EngineClient</c> and rebuilds its view model.
    /// With the default <c>NavigationCacheMode</c> the old page Unloads (disposing its view model) and a new one is constructed.</summary> <returns>True when the Frame accepted the navigation.</returns>
    internal bool ReloadMainPage() => RootFrame.Navigate(typeof(MainPage));
}
