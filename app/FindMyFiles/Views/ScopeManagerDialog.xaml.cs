using FindMyFiles.Services;
using FindMyFiles.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace FindMyFiles.Views;

/// <summary>
/// Wiring only: the scope-folder dialog (ADR-0024). Two contexts share it — the
/// setup screen's "no admin?" link (first-run onboarding, folders only) and the
/// gear menu's "Change search folders…" (re-selection, folders + excludes).
/// State and the persist/relaunch live on the shared <see cref="MainViewModel"/>;
/// the buttons drive its actions through the sanctioned
/// <see cref="FindMyFiles.Services.TaskExtensions.Forget"/> funnel (CLAUDE.md
/// convention). The mirror of <see cref="ServiceManagerDialog"/> for the
/// non-elevated path.
/// </summary>
// View code-behind: dialog wiring, not unit-tested (ADR-0022).
[System.Diagnostics.CodeAnalysis.ExcludeFromCodeCoverage]
public sealed partial class ScopeManagerDialog : ContentDialog
{
    private static bool _open;

    /// <summary>The page's ViewModel, shared so the dialog edits the same
    /// <see cref="MainViewModel.ScopeFolders"/> the setup screen seeds.</summary>
    public MainViewModel VM { get; }

    /// <summary>Whether the excludes section shows. False in the setup
    /// (first-run) context — excludes come later, once indexing has started, via
    /// the gear's "change search folders"; true for re-selection.</summary>
    public bool ShowExcludes { get; }

    private ScopeManagerDialog(MainViewModel vm, bool setup)
    {
        VM = vm;
        ShowExcludes = !setup;
        InitializeComponent();
        if (setup)
        {
            // First-run wording; the x:Uid defaults ("Search folders" / "Apply
            // and restart") read as a re-selection, not onboarding.
            Title = Loc.Get("ScopeDialog_SetupTitle");
            PrimaryButtonText = Loc.Get("ScopeDialog_SetupStart");
        }
    }

    /// <summary>The single entry point (setup link / gear menu). Resolves a
    /// XamlRoot from the main window and guards against a second instance
    /// (ContentDialog allows only one open at a time). Named OpenAsync, not
    /// ShowAsync, to avoid hiding the inherited
    /// <see cref="ContentDialog.ShowAsync()"/>.</summary>
    /// <param name="vm">The page ViewModel to edit.</param>
    /// <param name="setup">True for the first-run setup context (folders only,
    /// onboarding wording); false for re-selection (folders + excludes).</param>
    /// <returns>A <see cref="Task"/> that completes when the dialog closes.</returns>
    public static async Task OpenAsync(MainViewModel vm, bool setup = false)
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
            await new ScopeManagerDialog(vm, setup) { XamlRoot = root }.ShowAsync();
        }
        catch (Exception ex)
        {
            FileLog.Error("scope-ui", "scope manager dialog failed", ex);
            Notifier.Post(NotifySeverity.Warning, Loc.Get("Scope_OpenFailed"), ex.Message);
        }
        finally
        {
            _open = false;
        }
    }

    // Picks one folder per click (single-select OS dialog), adding it to the
    // shared ScopeFolders with case-insensitive dedupe.
    private void AddFolder_Click(object sender, RoutedEventArgs e) =>
        VM.PickScopeFoldersAsync().Forget("scope-ui");

    private void RemoveFolder_Click(object sender, RoutedEventArgs e)
    {
        if (sender is FrameworkElement { Tag: string path })
        {
            VM.RemoveScopeFolder(path);
        }
    }

    // Picks one subfolder to prune from the walk (ADR-0025); the VM rejects it
    // with a notice when it is not inside a selected root.
    private void AddExclude_Click(object sender, RoutedEventArgs e) =>
        VM.PickScopeExcludeAsync().Forget("scope-ui");

    private void RemoveExclude_Click(object sender, RoutedEventArgs e)
    {
        if (sender is FrameworkElement { Tag: string path })
        {
            VM.RemoveScopeExclude(path);
        }
    }

    // Primary button: normalize, persist, and relaunch into the new scope.
    // No-op (just closes) when nothing changed.
    private void Apply_Click(ContentDialog sender, ContentDialogButtonClickEventArgs args) =>
        VM.ApplyScopeChange();
}
