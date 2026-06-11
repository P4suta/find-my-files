using Microsoft.UI.Xaml;

namespace FindMyFiles;

/// <summary>The application window: hosts the root Frame; app UI lives in
/// MainPage.</summary>
public sealed partial class MainWindow : Window
{
    public MainWindow()
    {
        InitializeComponent();

        ExtendsContentIntoTitleBar = true;
        SetTitleBar(AppTitleBar);

        AppWindow.SetIcon("Assets/AppIcon.ico");

        RootFrame.Navigate(typeof(MainPage));
    }
}
