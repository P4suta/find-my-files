using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Windows.ApplicationModel.DataTransfer;
using Windows.Storage;
using Windows.System;
using FindMyFiles.Controls;
using FindMyFiles.Engine;
using FindMyFiles.Services;
using FindMyFiles.ViewModels;

namespace FindMyFiles;

/// <summary>
/// Wiring only: builds the ViewModel graph and connects view events to it.
/// Imperative ListView work (viewport/selection restore, row actions) lives
/// in <see cref="ResultsViewportManager"/>; the F12 panel chrome in
/// <see cref="Views.PerfPanel"/>; converters in
/// <see cref="Converters.UiConverters"/>.
/// </summary>
public sealed partial class MainPage : Page
{
    public MainViewModel ViewModel { get; }

    private readonly ResultsViewportManager _viewport;

    public MainPage()
    {
        ViewModel = new MainViewModel(
            App.EngineClient, new DispatcherQueueDispatcher(App.DispatcherQueue));
        InitializeComponent();
        _viewport = new ResultsViewportManager(ResultsList);
        ViewModel.Results.ResultsPublished += _viewport.OnResultsPublished;
        // IME: half-composed text (romaji fragments, candidate strings)
        // must not query — search the final string on commit/cancel.
        SearchBox.TextCompositionStarted += (_, _) => ViewModel.Search.NotifyCompositionStarted();
        SearchBox.TextCompositionEnded += (_, _) =>
            ViewModel.Search.NotifyCompositionEnded(ViewModel.SearchText);
        // 空クエリ=中央の大検索バー(Empty)、入力で上へ移動して結果を出す(Results)。
        ViewModel.PropertyChanged += OnViewModelPropertyChanged;
        Loaded += (_, _) =>
        {
            UpdateSearchState(useTransitions: false);
            SearchBox.Focus(Microsoft.UI.Xaml.FocusState.Programmatic);
            ViewModel.StartAsync().Forget("startup");
        };
    }

    private void OnViewModelPropertyChanged(
        object? sender, System.ComponentModel.PropertyChangedEventArgs e)
    {
        if (e.PropertyName == nameof(MainViewModel.SearchText))
        {
            UpdateSearchState(useTransitions: true);
        }
    }

    /// <summary>Empty(中央の大きな検索バーのみ) ↔ Results(検索バー上＋結果)。
    /// SearchText の空/非空で切り替える。ContentHost の RepositionThemeTransition
    /// が検索バーの移動を、ListView の AddDeleteThemeTransition が結果の出現を
    /// 滑らかにする(仮想化はコンテナ実体化時のみ走り、Reset とは競合しない)。</summary>
    private void UpdateSearchState(bool useTransitions)
    {
        var state = string.IsNullOrEmpty(ViewModel.SearchText) ? "EmptyState" : "ResultsState";
        Microsoft.UI.Xaml.VisualStateManager.GoToState(this, state, useTransitions);
    }

    private void Notification_Closed(InfoBar sender, InfoBarClosedEventArgs args)
    {
        if (sender.DataContext is AppNotification n)
        {
            ViewModel.Notifications.Remove(n);
        }
    }

    // 設定(歯車)フライアウト → 「診断情報を表示/隠す」で診断パネルを開閉。
    private void PerfPanel_MenuClick(object sender, Microsoft.UI.Xaml.RoutedEventArgs e) =>
        ViewModel.Perf.Toggle();

    // ── Drag & drop: folder → path: filter, file → name search ──────────

    private void Page_DragOver(object sender, Microsoft.UI.Xaml.DragEventArgs e)
    {
        if (e.DataView.Contains(StandardDataFormats.StorageItems))
        {
            e.AcceptedOperation = DataPackageOperation.Link;
            if (e.DragUIOverride is { } ui)
            {
                ui.Caption = "検索条件として追加";
            }
        }
    }

    /// <summary>Drop-in only (rows are not drag-out sources). Anything that
    /// goes wrong is logged and swallowed — a failed drop must never take
    /// the app down (落ちない).</summary>
    private async void Page_Drop(object sender, Microsoft.UI.Xaml.DragEventArgs e)
    {
        var deferral = e.GetDeferral();
        try
        {
            if (!e.DataView.Contains(StandardDataFormats.StorageItems))
            {
                return;
            }
            var items = await e.DataView.GetStorageItemsAsync();
            var item = items.FirstOrDefault();
            if (item is null)
            {
                return;
            }
            if (item.IsOfType(StorageItemTypes.Folder))
            {
                // Scope the current query to the dropped folder.
                ViewModel.SearchText = $"path:\"{item.Path}\" " + ViewModel.SearchText;
            }
            else
            {
                ViewModel.SearchText = item.Name;
            }
        }
        catch (Exception ex)
        {
            FileLog.Error("dragdrop", "drop handling failed", ex);
        }
        finally
        {
            deferral.Complete();
        }
    }

    // ── Keyboard / pointer / menu → viewport manager and ViewModel ──────

    private void SearchBox_KeyDown(object sender, KeyRoutedEventArgs e)
    {
        switch (e.Key)
        {
            case VirtualKey.Down:
                _viewport.FocusTopRow();
                e.Handled = true;
                break;
            case VirtualKey.Enter:
                _viewport.OpenSelectedOrTop();
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
                _viewport.RevealSelected();
                e.Handled = true;
                break;
            case VirtualKey.Enter:
                _viewport.OpenSelected();
                e.Handled = true;
                break;
            case VirtualKey.C when ctrl:
                _viewport.CopySelectedPaths();
                e.Handled = true;
                break;
            case VirtualKey.Escape:
                SearchBox.Focus(Microsoft.UI.Xaml.FocusState.Programmatic);
                SearchBox.SelectAll();
                e.Handled = true;
                break;
        }
    }

    private void ResultsList_DoubleTapped(object sender, DoubleTappedRoutedEventArgs e) =>
        _viewport.OpenSelected();

    private void MenuOpen_Click(object sender, Microsoft.UI.Xaml.RoutedEventArgs e) =>
        _viewport.OpenSelected();

    private void MenuOpenPath_Click(object sender, Microsoft.UI.Xaml.RoutedEventArgs e) =>
        _viewport.RevealSelected();

    private void MenuCopyPath_Click(object sender, Microsoft.UI.Xaml.RoutedEventArgs e) =>
        _viewport.CopySelectedPaths();

    private void HeaderName_Click(object sender, Microsoft.UI.Xaml.RoutedEventArgs e) =>
        ViewModel.SetSort(FmfSort.Name);

    private void HeaderSize_Click(object sender, Microsoft.UI.Xaml.RoutedEventArgs e) =>
        ViewModel.SetSort(FmfSort.Size);

    private void HeaderDate_Click(object sender, Microsoft.UI.Xaml.RoutedEventArgs e) =>
        ViewModel.SetSort(FmfSort.Mtime);
}
