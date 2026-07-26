using FindMyFiles.Engine;
using FindMyFiles.Services;
using FindMyFiles.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;

namespace FindMyFiles.Views;

/// <summary>
/// Wiring only: the settings / status / diagnostics surface (the gear button's
/// content, rebuilt from a flat <c>MenuFlyout</c> into a Fluent settings page).
/// Toggles two-way bind to the shared <see cref="MainViewModel"/> (its
/// OnXxxChanged handlers persist + requery); sort, regex scope and language go
/// through the same VM methods the old menu used. Actions that open another
/// surface (diagnostics window or service dialog) close this dialog first
/// and run once it is fully dismissed — only one <see cref="ContentDialog"/> may
/// be open per <c>XamlRoot</c>. The peer of <see cref="ServiceManagerDialog"/>.
/// </summary>
// View code-behind: dialog wiring, not unit-tested (ADR-0022).
[System.Diagnostics.CodeAnalysis.ExcludeFromCodeCoverage]
public sealed partial class SettingsDialog : ContentDialog
{
    private static bool _open;

    /// <summary>The page's ViewModel, shared so the dialog drives the same
    /// search/filter/sort state the search box and result list read.</summary>
    internal MainViewModel VM { get; }

    /// <summary>An action queued by a card that opens another surface; run after
    /// this dialog is dismissed so the next ContentDialog/window opens cleanly.</summary>
    private Action? _pendingAction;
    private bool _restoringLanguage;

    private SettingsDialog(MainViewModel vm)
    {
        VM = vm;
        InitializeComponent();
        OptFocusedCard.Header = Loc.GetXaml("OptFocused", "Header");
        OptFocusedCard.Description = Loc.GetXaml("OptFocused", "Description");
        OptSystemCard.Header = Loc.GetXaml("OptSystem", "Header");
        OptRegexCard.Header = Loc.GetXaml("OptRegex", "Header");
        OptRegexCard.Description = Loc.GetXaml("OptRegex", "Description");
        RegexScopeCard.Header = Loc.GetXaml("RegexScopeMenu", "Header");
        SortFieldCard.Header = Loc.GetXaml("SortHeaderItem", "Header");
        SortDescendingCard.Header = Loc.GetXaml("SortDescendingCard", "Header");
        LanguageCard.Header = Loc.GetXaml("LangMenu", "Header");
        CloseToTrayCard.Header = Loc.GetXaml("OptCloseToTray", "Header");
        CloseToTrayCard.Description = Loc.GetXaml("OptCloseToTray", "Description");
        DiagnosticsCard.Header = Loc.GetXaml("DiagCard", "Header");
        DiagnosticsCard.Description = Loc.GetXaml("DiagCard", "Description");
        ServiceCard.Header = Loc.GetXaml("ServiceCard", "Header");
        ServiceCard.Description = Loc.GetXaml("ServiceCard", "Description");
        AutomationProperties.SetName(OptFocusedToggle, Loc.GetXaml("OptFocused", "Header"));
        AutomationProperties.SetName(OptSystemToggle, Loc.GetXaml("OptSystem", "Header"));
        AutomationProperties.SetName(OptRegexToggle, Loc.GetXaml("OptRegex", "Header"));
        AutomationProperties.SetName(
            SortDescendingToggle,
            Loc.GetXaml("SortDescendingCard", "Header"));
        AutomationProperties.SetName(LangCombo, Loc.GetXaml("LangMenu", "Header"));
        AutomationProperties.SetName(CloseToTrayToggle, Loc.GetXaml("OptCloseToTray", "Header"));

        // Reflect persisted state into the controls that can't two-way bind
        // (radios drive VM methods, the language combo relaunches). Setting
        // RadioButton.IsChecked does not raise Click, so this does not loop back.
        (VM.RegexScope == RegexScope.Path ? RegexScopePath : RegexScopeName).IsChecked = true;
        (VM.Sort switch
        {
            FmfSort.Size => SortSize,
            FmfSort.Mtime => SortDate,
            _ => SortName,
        }).IsChecked = true;

        // SelectedItem raises SelectionChanged, but the handler no-ops when the
        // language is unchanged (which it is here), so this never relaunches.
        LangCombo.SelectedItem = AppSettings.Load().Language switch
        {
            "ja" => LangJa,
            "en" => LangEn,
            "zh-Hans" => LangZh,
            _ => LangAuto,
        };
    }

    /// <summary>The single entry point (the gear button). Resolves a XamlRoot from
    /// the main window and guards against a second instance (one ContentDialog per
    /// XamlRoot). Named OpenAsync, not ShowAsync, to avoid hiding the inherited
    /// <see cref="ContentDialog.ShowAsync()"/>. Any action queued by a card runs
    /// once the dialog has fully closed.</summary>
    /// <param name="vm">The page ViewModel to drive.</param>
    /// <returns>A <see cref="Task"/> that completes when the dialog closes.</returns>
    internal static async Task OpenAsync(MainViewModel vm)
    {
        if (_open)
        {
            return;
        }

        var root = App.Window?.Content?.XamlRoot;
        if (root is null)
        {
            return;
        }

        _open = true;
        try
        {
            var dialog = new SettingsDialog(vm) { XamlRoot = root };

            // Populate the About block's engine version as the dialog appears
            // (best-effort; the bound rows fill in when it resolves).
            vm.RefreshVersionsAsync().Forget("settings-version");
            await dialog.ShowAsync();

            // Closed now — safe to open the next surface (another ContentDialog
            // would throw while this one is still up).
            dialog._pendingAction?.Invoke();
        }
        catch (Exception ex)
        {
            FileLog.Error("settings-ui", "settings dialog failed", ex);
            Notifier.Post(NotifySeverity.Warning, Loc.Get("Settings_OpenFailed"), ex.Message);
        }
        finally
        {
            _open = false;
        }
    }

    private void RegexScopeName_Click(object sender, RoutedEventArgs e) =>
        VM.RegexScope = RegexScope.Name;

    private void RegexScopePath_Click(object sender, RoutedEventArgs e) =>
        VM.RegexScope = RegexScope.Path;

    private void SortName_Click(object sender, RoutedEventArgs e) =>
        VM.SetSort(FmfSort.Name);

    private void SortSize_Click(object sender, RoutedEventArgs e) =>
        VM.SetSort(FmfSort.Size);

    private void SortDate_Click(object sender, RoutedEventArgs e) =>
        VM.SetSort(FmfSort.Mtime);

    private void SortDescending_Toggled(object sender, RoutedEventArgs e)
    {
        if (sender is ToggleSwitch toggle)
        {
            VM.SetSortDescending(toggle.IsOn);
        }
    }

    // Language switch: persist to settings.json and do a true process restart so
    // the App ctor re-applies PrimaryLanguageOverride and the window chrome rebuilds
    // (an in-process soft restart only rebuilds the page body). The restart goes
    // through AppInstance.Restart so it survives single-instancing (ADR-0036). Tag
    // is "auto"/"ja"/"en"/"zh-Hans"; no-ops when unchanged, so the ctor's initial
    // selection never restarts.
    private void Language_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_restoringLanguage
            || LangCombo.SelectedItem is not FrameworkElement { Tag: string lang })
        {
            return;
        }

        var settings = AppSettings.Load();
        if (string.Equals(settings.Language, lang, StringComparison.Ordinal))
        {
            return;
        }

        var previous = settings.Language;
        if (TryPersistLanguage(settings, lang))
        {
            ShellOps.Relaunch();
            return;
        }

        _restoringLanguage = true;
        try
        {
            LangCombo.SelectedItem = LanguageItem(previous);
        }
        finally
        {
            _restoringLanguage = false;
        }

        Notifier.Post(
            NotifySeverity.Error,
            Loc.Get("Settings_SaveFailedTitle"),
            Loc.Get("Settings_LanguageSaveFailedBody"));
    }

    /// <summary>Persist a language change transactionally: a failed save restores
    /// the in-memory value so callers never restart into a language that was not
    /// durably selected.</summary>
    /// <param name="settings">Settings instance to mutate.</param>
    /// <param name="language">Normalized language tag selected by the UI.</param>
    /// <param name="save">Optional persistence seam for tests.</param>
    /// <returns>True only when the new value was saved.</returns>
    internal static bool TryPersistLanguage(
        AppSettings settings,
        string language,
        Func<bool>? save = null)
    {
        var previous = settings.Language;
        settings.Language = language;
        if ((save ?? settings.Save)())
        {
            return true;
        }

        settings.Language = previous;
        return false;
    }

    private ComboBoxItem LanguageItem(string language) => language switch
    {
        "ja" => LangJa,
        "en" => LangEn,
        "zh-Hans" => LangZh,
        _ => LangAuto,
    };

    // The next two close this dialog, then open their surface once it is gone.
    private void Diag_Click(object sender, RoutedEventArgs e)
    {
        _pendingAction = () => App.ToggleDiagnostics(VM.Perf);
        Hide();
    }

    private void Service_Click(object sender, RoutedEventArgs e)
    {
        _pendingAction = () => ServiceManagerDialog.OpenAsync().Forget("service-ui");
        Hide();
    }
}
