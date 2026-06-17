using FindMyFiles.Controls;
using FindMyFiles.Engine;
using FindMyFiles.Services;
using FindMyFiles.ViewModels;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Windows.ApplicationModel.DataTransfer;
using Windows.Storage;
using Windows.System;

namespace FindMyFiles;

/// <summary>
/// Wiring only: builds the ViewModel graph and connects view events to it.
/// Imperative ListView work (viewport/selection restore, row actions) lives
/// in <see cref="ResultsViewportManager"/>; the F12 panel chrome in
/// <see cref="Views.PerfPanel"/>; converters in
/// <see cref="Converters.UiConverters"/>.
/// </summary>
// View code-behind: imperative ListView/keyboard/menu wiring, not unit-tested (ADR-0022).
[System.Diagnostics.CodeAnalysis.ExcludeFromCodeCoverage]
public sealed partial class MainPage : Page
{
    /// <summary>Root of the page's ViewModel graph. The sole `x:Bind` source;
    /// it ties together the search, results, notification and diagnostics-panel
    /// sub-ViewModels.</summary>
    public MainViewModel ViewModel { get; }

    private readonly ResultsViewportManager _viewport;

    /// <summary>Builds the ViewModel graph and wires view events (IME composition,
    /// drag &amp; drop, keyboard, sort headers) to the ViewModel and
    /// <see cref="ResultsViewportManager"/>. Localized tooltips/automation names
    /// are set in code here, and the language radio reflects persisted settings.
    /// Finally initializes the empty/results visual state and starts `StartAsync`.</summary>
    public MainPage()
    {
        ViewModel = new MainViewModel(
            App.EngineClient, new DispatcherQueueDispatcher(App.DispatcherQueue));
        InitializeComponent();

        // Attached properties (tooltip / accessibility name) localize in code —
        // simpler than the x:Uid attached-property resw syntax.
        ToolTipService.SetToolTip(OptionsButton, Loc.Get("OptionsButton_ToolTip"));
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(
            OptionsButton, Loc.Get("OptionsButton_Name"));

        // Reflect the persisted UI language in the switcher's radio group.
        (AppSettings.Load().Language switch
        {
            "ja" => LangJa,
            "en" => LangEn,
            "zh-Hans" => LangZh,
            _ => LangAuto,
        }).IsChecked = true;

        // Reflect the restored regex scope (the ViewModel already loaded it)
        // in the radio group. Setting IsChecked programmatically does not fire
        // Click, so this does not loop back into the ViewModel.
        (ViewModel.RegexScope == RegexScope.Path ? RegexScopePath : RegexScopeName).IsChecked = true;
        _viewport = new ResultsViewportManager(ResultsList);
        ViewModel.Results.ResultsPublished += _viewport.OnResultsPublished;

        // IME: half-composed text (romaji fragments, candidate strings)
        // must not query — search the final string on commit/cancel.
        SearchBox.TextCompositionStarted += (_, _) => ViewModel.Search.NotifyCompositionStarted();
        SearchBox.TextCompositionEnded += (_, _) =>
            ViewModel.Search.NotifyCompositionEnded(ViewModel.SearchText);

        // Empty query = large centered search bar (Empty); on input it moves up
        // and shows results (Results).
        ViewModel.PropertyChanged += OnViewModelPropertyChanged;
        Loaded += (_, _) =>
        {
            UpdateSearchState(useTransitions: false);

            // Disconnected → the search box is collapsed; focus the setup CTA.
            if (ViewModel.IsReady)
            {
                SearchBox.Focus(Microsoft.UI.Xaml.FocusState.Programmatic);
            }
            else
            {
                EnableSearchButton.Focus(Microsoft.UI.Xaml.FocusState.Programmatic);
            }

            ViewModel.StartAsync().Forget("startup");
        };
    }

    private void OnViewModelPropertyChanged(
        object? sender, System.ComponentModel.PropertyChangedEventArgs e)
    {
        if (string.Equals(e.PropertyName, nameof(MainViewModel.SearchText), StringComparison.Ordinal))
        {
            UpdateSearchState(useTransitions: true);
        }
    }

    /// <summary>Empty (large centered search bar only) ↔ Results (search bar on
    /// top + results). Switches on whether SearchText is empty. ContentHost's
    /// RepositionThemeTransition smooths the search bar's move and the ListView's
    /// AddDeleteThemeTransition smooths the results' appearance (virtualization
    /// runs only on container realization and does not conflict with Reset).</summary>
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

    // Settings (gear) flyout → "show/hide diagnostics" toggles the diagnostics panel.
    private void PerfPanel_MenuClick(object sender, Microsoft.UI.Xaml.RoutedEventArgs e) =>
        ViewModel.Perf.Toggle();

    // Settings (gear) flyout → "Manage service…". Calls the shared entry point
    // (which contains the single-dialog guard + failure notification).
    private void ServiceManager_MenuClick(object sender, Microsoft.UI.Xaml.RoutedEventArgs e) =>
        Views.ServiceManagerDialog.OpenAsync().Forget("service-ui");

    // Settings (gear) flyout → "Change search folders…" (scope mode, ADR-0024).
    // The mirror of ServiceManager_MenuClick for the non-elevated path.
    private void ScopeManager_MenuClick(object sender, Microsoft.UI.Xaml.RoutedEventArgs e) =>
        Views.ScopeManagerDialog.OpenAsync(ViewModel).Forget("scope-ui");

    // Primary button of the disconnected setup screen → one-click register → auto relaunch.
    private void EnableSearch_Click(object sender, Microsoft.UI.Xaml.RoutedEventArgs e) =>
        ViewModel.EnableSearchAsync().Forget("service-ui");

    // Setup screen scope path (no admin, ADR-0024): pick folders → save →
    // relaunch enters WalkInProc.
    private void PickScopeFolders_Click(object sender, Microsoft.UI.Xaml.RoutedEventArgs e) =>
        ViewModel.PickScopeFoldersAsync().Forget("scope-ui");

    private void StartScopeSearch_Click(object sender, Microsoft.UI.Xaml.RoutedEventArgs e) =>
        ViewModel.StartScopeSearch();

    private void RemoveScopeFolder_Click(object sender, Microsoft.UI.Xaml.RoutedEventArgs e)
    {
        if (sender is Microsoft.UI.Xaml.FrameworkElement { Tag: string path })
        {
            ViewModel.RemoveScopeFolder(path);
        }
    }

    // Language switch: persist to settings.json and relaunch the app (App ctor
    // applies PrimaryLanguageOverride). Tag is "auto"/"ja"/"en"/"zh-Hans".
    private void Language_Click(object sender, Microsoft.UI.Xaml.RoutedEventArgs e)
    {
        if (sender is not MenuFlyoutItemBase { Tag: string lang })
        {
            return;
        }

        var settings = AppSettings.Load();
        if (string.Equals(settings.Language, lang, StringComparison.Ordinal))
        {
            return;
        }

        settings.Language = lang;
        settings.Save();
        ShellOps.Relaunch();
    }

    // ── Drag & drop: folder → path: filter, file → name search ──────────
    private void Page_DragOver(object sender, Microsoft.UI.Xaml.DragEventArgs e)
    {
        if (e.DataView.Contains(StandardDataFormats.StorageItems))
        {
            e.AcceptedOperation = DataPackageOperation.Link;
            if (e.DragUIOverride is { } ui)
            {
                ui.Caption = Loc.Get("Drag_AddAsCondition");
            }
        }
    }

    /// <summary>Drop-in only (rows are not drag-out sources). Anything that
    /// goes wrong is logged and swallowed — a failed drop must never take
    /// the app down (don't crash).</summary>
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
            var item = items.Count > 0 ? items[0] : null;
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
            & Windows.UI.Core.CoreVirtualKeyStates.Down) != Windows.UI.Core.CoreVirtualKeyStates.None;
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

    private void RegexScopeName_Click(object sender, Microsoft.UI.Xaml.RoutedEventArgs e) =>
        ViewModel.RegexScope = RegexScope.Name;

    private void RegexScopePath_Click(object sender, Microsoft.UI.Xaml.RoutedEventArgs e) =>
        ViewModel.RegexScope = RegexScope.Path;
}
