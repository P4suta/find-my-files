using System.Diagnostics;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Windows.ApplicationModel.DataTransfer;
using Windows.System;
using FindMyFiles.Engine;
using FindMyFiles.ViewModels;

namespace FindMyFiles;

public sealed partial class MainPage : Page
{
    public MainViewModel ViewModel { get; }

    public MainPage()
    {
        ViewModel = new MainViewModel(App.EngineClient, App.DispatcherQueue);
        InitializeComponent();
        Loaded += (_, _) =>
        {
            SearchBox.Focus(Microsoft.UI.Xaml.FocusState.Programmatic);
            ViewModel.Start();
        };
    }

    private void SearchBox_KeyDown(object sender, KeyRoutedEventArgs e)
    {
        switch (e.Key)
        {
            case VirtualKey.Down:
                ResultsList.Focus(Microsoft.UI.Xaml.FocusState.Programmatic);
                if (ResultsList.Items.Count > 0)
                {
                    ResultsList.SelectedIndex = 0;
                    ResultsList.ScrollIntoView(ResultsList.SelectedItem);
                }
                e.Handled = true;
                break;
            case VirtualKey.Enter:
                OpenRow(FirstSelectedOrTop());
                e.Handled = true;
                break;
            case VirtualKey.Escape:
                ViewModel.SearchText = string.Empty;
                e.Handled = true;
                break;
        }
    }

    private void ResultsList_KeyDown(object sender, KeyRoutedEventArgs e)
    {
        var ctrl = (Microsoft.UI.Input.InputKeyboardSource
            .GetKeyStateForCurrentThread(VirtualKey.Control)
            & Windows.UI.Core.CoreVirtualKeyStates.Down) != 0;
        switch (e.Key)
        {
            case VirtualKey.Enter when ctrl:
                RevealRow(SelectedRow());
                e.Handled = true;
                break;
            case VirtualKey.Enter:
                OpenRow(SelectedRow());
                e.Handled = true;
                break;
            case VirtualKey.C when ctrl:
                CopySelectedPaths();
                e.Handled = true;
                break;
            case VirtualKey.Escape:
                SearchBox.Focus(Microsoft.UI.Xaml.FocusState.Programmatic);
                SearchBox.SelectAll();
                e.Handled = true;
                break;
        }
    }

    private ResultRow? SelectedRow() => ResultsList.SelectedItem as ResultRow;

    private ResultRow? FirstSelectedOrTop() =>
        SelectedRow() ?? (ResultsList.Items.Count > 0 ? ResultsList.Items[0] as ResultRow : null);

    private void ResultsList_DoubleTapped(object sender, DoubleTappedRoutedEventArgs e) =>
        OpenRow(SelectedRow());

    private void MenuOpen_Click(object sender, Microsoft.UI.Xaml.RoutedEventArgs e) =>
        OpenRow(SelectedRow());

    private void MenuOpenPath_Click(object sender, Microsoft.UI.Xaml.RoutedEventArgs e) =>
        RevealRow(SelectedRow());

    private void MenuCopyPath_Click(object sender, Microsoft.UI.Xaml.RoutedEventArgs e) =>
        CopySelectedPaths();

    /// <summary>
    /// Open via explorer.exe so the target launches *unelevated* — launching
    /// directly from this admin process would elevate the associated app
    /// (CLAUDE.md UI固定則).
    /// </summary>
    private static void OpenRow(ResultRow? row)
    {
        if (row is null || row.IsPlaceholder)
        {
            return;
        }
        Process.Start(new ProcessStartInfo
        {
            FileName = "explorer.exe",
            Arguments = $"\"{row.FullPath}\"",
            UseShellExecute = false,
        });
    }

    private static void RevealRow(ResultRow? row)
    {
        if (row is null || row.IsPlaceholder)
        {
            return;
        }
        Process.Start(new ProcessStartInfo
        {
            FileName = "explorer.exe",
            Arguments = $"/select,\"{row.FullPath}\"",
            UseShellExecute = false,
        });
    }

    private void CopySelectedPaths()
    {
        var paths = ResultsList.SelectedItems
            .OfType<ResultRow>()
            .Where(r => !r.IsPlaceholder)
            .Select(r => r.FullPath)
            .ToList();
        if (paths.Count == 0)
        {
            return;
        }
        var pkg = new DataPackage();
        pkg.SetText(string.Join("\r\n", paths));
        Clipboard.SetContent(pkg);
    }

    private void HeaderName_Click(object sender, Microsoft.UI.Xaml.RoutedEventArgs e) =>
        ViewModel.SetSort(FmfSort.Name);

    private void HeaderSize_Click(object sender, Microsoft.UI.Xaml.RoutedEventArgs e) =>
        ViewModel.SetSort(FmfSort.Size);

    private void HeaderDate_Click(object sender, Microsoft.UI.Xaml.RoutedEventArgs e) =>
        ViewModel.SetSort(FmfSort.Mtime);
}
