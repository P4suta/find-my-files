using Microsoft.UI.Xaml;

namespace FindMyFiles;

/// <summary>The application window: hosts the root Frame; app UI lives in
/// MainPage.</summary>
// View shell: window chrome + title bar, not unit-tested (ADR-0022).
[System.Diagnostics.CodeAnalysis.ExcludeFromCodeCoverage]
public sealed partial class MainWindow : Window
{
    /// <summary>タイトルバーをコンテンツへ拡張し、ウィンドウアイコンを設定したうえで
    /// ルート Frame を <see cref="MainPage"/> へナビゲートする。</summary>
    public MainWindow()
    {
        InitializeComponent();

        ExtendsContentIntoTitleBar = true;
        SetTitleBar(AppTitleBar);

        AppWindow.SetIcon("Assets/AppIcon.ico");

        RootFrame.Navigate(typeof(MainPage));
    }
}
