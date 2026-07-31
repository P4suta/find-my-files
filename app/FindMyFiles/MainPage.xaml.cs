using FindMyFiles.Controls;
using FindMyFiles.Services;
using FindMyFiles.ViewModels;
using Microsoft.UI.Xaml.Automation.Peers;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
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
    internal MainViewModel ViewModel { get; }

    private readonly ResultsViewportManager _viewport;
    private bool _disposed;
    private bool _initialPrimaryFocusPending = true;

    /// <summary>Builds the ViewModel graph and wires view events (IME composition,
    /// drag &amp; drop, keyboard, sort headers) to the ViewModel and
    /// <see cref="ResultsViewportManager"/>. Localized tooltips/automation names
    /// are set in code here, and the language radio reflects persisted settings.
    /// Finally initializes the empty/results visual state and starts `StartAsync`.</summary>
    public MainPage()
    {
        ViewModel = new MainViewModel(
            App.EngineClient,
            new DispatcherQueueDispatcher(App.DispatcherQueue),
            openServiceManager: OpenServiceManager);
        InitializeComponent();

        // Attached properties (tooltip / accessibility name) localize in code —
        // simpler than the x:Uid attached-property resw syntax.
        ToolTipService.SetToolTip(OptionsButton, Loc.Get("OptionsButton_ToolTip"));
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(
            OptionsButton, Loc.Get("OptionsButton_Name"));
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(
            ResultsList, Loc.Get("ResultsList_Name"));
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(
            SetupTitleText, Loc.GetXaml("SetupTitle", "Text"));

        _viewport = new ResultsViewportManager(ResultsList);
        ViewModel.Results.ResultsPublished += _viewport.OnResultsPublished;

        // IME: half-composed text (romaji fragments, candidate strings)
        // must not query — search the final string on commit/cancel.
        SearchBox.TextCompositionStarted += (_, _) => ViewModel.Search.NotifyCompositionStarted();
        SearchBox.TextCompositionEnded += (_, _) =>
            ViewModel.Search.NotifyCompositionEnded(ViewModel.SearchText);

        // A soft restart (ADR-0036) re-navigates the Frame to a fresh MainPage; the
        // Frame does not dispose the page it replaces, so release the old view
        // model's engine-event subscriptions here. The disposal is idempotent, so
        // it coexists with the Window.Closed engine dispose in App.
        Unloaded += OnUnloaded;

        // Empty query = large centered search bar (Empty); on input it moves up
        // and shows results (Results).
        ViewModel.PropertyChanged += OnViewModelPropertyChanged;
        Loaded += (_, _) =>
        {
            UpdateSearchState(useTransitions: false);

            // Loaded precedes WinUI's final custom-title-bar focus assignment on
            // some launches. Defer one dispatcher turn so our primary control is
            // the last intentional focus decision, not transient window chrome.
            DispatcherQueue.TryEnqueue(
                () => UpdateAvailabilityFocusAndAnnouncement(
                    announceSetup: ViewModel.IsDisconnected,
                    forcePrimaryFocus: true));
            ViewModel.StartAsync().Forget("startup");
        };
    }

    private void OnUnloaded(object sender, Microsoft.UI.Xaml.RoutedEventArgs e)
    {
        if (_disposed)
        {
            return;
        }

        _disposed = true;
        Unloaded -= OnUnloaded;
        ViewModel.PropertyChanged -= OnViewModelPropertyChanged;
        ViewModel.Results.ResultsPublished -= _viewport.OnResultsPublished;
        ViewModel.Dispose();
    }

    private void OnViewModelPropertyChanged(
        object? sender, System.ComponentModel.PropertyChangedEventArgs e)
    {
        if (string.Equals(e.PropertyName, nameof(MainViewModel.SearchText), StringComparison.Ordinal))
        {
            UpdateSearchState(useTransitions: true);
        }
        else if (string.Equals(
            e.PropertyName,
            nameof(MainViewModel.IsDisconnected),
            StringComparison.Ordinal))
        {
            // Defer until x:Bind has applied both sides' Visibility. Moving
            // focus before the old subtree collapses can leave the UIA focus
            // on an element that no longer exists visually.
            DispatcherQueue.TryEnqueue(
                () => UpdateAvailabilityFocusAndAnnouncement(announceSetup: true));
        }
        else if (string.Equals(
            e.PropertyName,
            nameof(MainViewModel.CanSearch),
            StringComparison.Ordinal))
        {
            DispatcherQueue.TryEnqueue(
                () => UpdateAvailabilityFocusAndAnnouncement(announceSetup: false));
        }
    }

    private void UpdateAvailabilityFocusAndAnnouncement(
        bool announceSetup,
        bool forcePrimaryFocus = false)
    {
        if (_disposed || XamlRoot is null)
        {
            return;
        }

        var focused = FocusManager.GetFocusedElement(XamlRoot) as Microsoft.UI.Xaml.DependencyObject;
        var mustMovePrimaryFocus =
            forcePrimaryFocus || _initialPrimaryFocusPending;
        var focusMoved = false;
        var focusTarget = string.Empty;
        if (ViewModel.IsDisconnected)
        {
            if (mustMovePrimaryFocus
                || focused is null
                || IsInside(focused, NotifyBar)
                || IsInside(focused, SearchArea)
                || IsInside(focused, ResultsArea))
            {
                focusTarget = "enable-search";
                focusMoved =
                    EnableSearchButton.Focus(Microsoft.UI.Xaml.FocusState.Programmatic);
            }

            if (announceSetup)
            {
                var peer = FrameworkElementAutomationPeer.FromElement(SetupTitleText)
                    ?? FrameworkElementAutomationPeer.CreatePeerForElement(SetupTitleText);
                peer.RaiseAutomationEvent(AutomationEvents.LiveRegionChanged);
            }
        }
        else if (mustMovePrimaryFocus
            || focused is null
            || IsInside(focused, SetupArea)
            || (ViewModel.CanSearch && ReferenceEquals(focused, OptionsButton)))
        {
            if (ViewModel.CanSearch)
            {
                focusTarget = "search";
                focusMoved = SearchBox.Focus(Microsoft.UI.Xaml.FocusState.Programmatic);
            }
            else
            {
                focusTarget = "options";
                focusMoved = OptionsButton.Focus(Microsoft.UI.Xaml.FocusState.Programmatic);
            }
        }
        else if (!ViewModel.CanSearch && ReferenceEquals(focused, SearchBox))
        {
            focusTarget = "options";
            focusMoved = OptionsButton.Focus(Microsoft.UI.Xaml.FocusState.Programmatic);
        }

        if (focusTarget.Length > 0)
        {
            FileLog.Event(
                "focus",
                "primary focus attempted",
                ("target", focusTarget),
                ("moved", focusMoved),
                ("initial_pending", _initialPrimaryFocusPending),
                ("previous_type", focused?.GetType().Name ?? "none"),
                ("disconnected", ViewModel.IsDisconnected),
                ("can_search", ViewModel.CanSearch));
        }

        // Resolving starts before either availability surface is focusable.
        // Preserve the one-shot handoff across those expected failed attempts;
        // after the first real primary control accepts focus, later state
        // transitions resume the normal "preserve user focus" policy above.
        if (focusMoved)
        {
            _initialPrimaryFocusPending = false;
        }
    }

    /// <summary>Called by the window after activation when global focus is still
    /// on window chrome. Content/dialog focus never enters this path.</summary>
    internal void FocusPrimaryAction() =>
        UpdateAvailabilityFocusAndAnnouncement(
            announceSetup: false,
            forcePrimaryFocus: true);

    private static bool IsInside(
        Microsoft.UI.Xaml.DependencyObject element,
        Microsoft.UI.Xaml.DependencyObject root)
    {
        for (Microsoft.UI.Xaml.DependencyObject? current = element;
             current is not null;
             current = VisualTreeHelper.GetParent(current))
        {
            if (ReferenceEquals(current, root))
            {
                return true;
            }
        }

        return false;
    }

    [System.Diagnostics.CodeAnalysis.SuppressMessage(
        "Performance",
        "CA1822:Mark members as static",
        Justification = "XAML event handlers must be instance methods")]
    private void ResultsList_ContainerContentChanging(
        ListViewBase sender,
        ContainerContentChangingEventArgs args)
    {
        if (args.ItemContainer is not ListViewItem container || args.Item is not ResultRow row)
        {
            return;
        }

        // Style-setter bindings are not reliably applied to recycled WinUI
        // ListViewItem automation peers. Bind explicitly to the current row so
        // every realized container has a stable position ID and receives the
        // completed screen-reader summary after its virtual page is filled.
        container.SetBinding(
            Microsoft.UI.Xaml.Automation.AutomationProperties.AutomationIdProperty,
            new Microsoft.UI.Xaml.Data.Binding
            {
                Source = row,
                Path = new Microsoft.UI.Xaml.PropertyPath(nameof(ResultRow.AutomationId)),
                Mode = Microsoft.UI.Xaml.Data.BindingMode.OneWay,
            });
        container.SetBinding(
            Microsoft.UI.Xaml.Automation.AutomationProperties.NameProperty,
            new Microsoft.UI.Xaml.Data.Binding
            {
                Source = row,
                Path = new Microsoft.UI.Xaml.PropertyPath(nameof(ResultRow.AutomationName)),
                Mode = Microsoft.UI.Xaml.Data.BindingMode.OneWay,
            });
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

    // Gear button → the settings / status / diagnostics dialog.
    private void OptionsButton_Click(object sender, Microsoft.UI.Xaml.RoutedEventArgs e) =>
        Views.SettingsDialog.OpenAsync(ViewModel).Forget("settings-ui");

    // Primary button of the disconnected setup screen → one-click register → auto relaunch.
    private void EnableSearch_Click(object sender, Microsoft.UI.Xaml.RoutedEventArgs e) =>
        ViewModel.EnableSearchAsync().Forget("service-ui");

    // Recovery must remain reachable when no engine can be resolved. The settings
    // surface owns diagnostics plus service repair/uninstall; unlike the search
    // gear this button is part of the disconnected setup screen itself.
    private void SetupRecovery_Click(object sender, Microsoft.UI.Xaml.RoutedEventArgs e) =>
        Views.SettingsDialog.OpenAsync(ViewModel).Forget("settings-ui");

    // Injected into MainViewModel's version-skew notification. Keeping this
    // coordination here prevents the ViewModel from referencing a WinUI view.
    private void OpenServiceManager() =>
        Views.ServiceManagerDialog.OpenAsync().Forget("service-ui");

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
}
