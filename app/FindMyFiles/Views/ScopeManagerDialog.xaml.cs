using FindMyFiles.Services;
using FindMyFiles.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace FindMyFiles.Views;

/// <summary>
/// Wiring only: the gear menu's "Change search folders…" dialog (scope mode,
/// ADR-0024). State and the persist/relaunch live on the shared
/// <see cref="MainViewModel"/>; the buttons drive its actions through the
/// sanctioned <see cref="FindMyFiles.Services.TaskExtensions.Forget"/> funnel
/// (CLAUDE.md convention). The mirror of <see cref="ServiceManagerDialog"/> for
/// the non-elevated path.
/// </summary>
// View code-behind: dialog wiring, not unit-tested (ADR-0022).
[System.Diagnostics.CodeAnalysis.ExcludeFromCodeCoverage]
public sealed partial class ScopeManagerDialog : ContentDialog
{
    private static bool _open;

    /// <summary>The page's ViewModel, shared so the dialog edits the same
    /// <see cref="MainViewModel.ScopeFolders"/> the setup screen seeds.</summary>
    public MainViewModel VM { get; }

    private ScopeManagerDialog(MainViewModel vm)
    {
        VM = vm;
        InitializeComponent();
    }

    /// <summary>The single entry point (the gear menu). Resolves a XamlRoot from
    /// the main window and guards against a second instance (ContentDialog allows
    /// only one open at a time). Named OpenAsync, not ShowAsync, to avoid hiding
    /// the inherited <see cref="ContentDialog.ShowAsync()"/>.</summary>
    /// <param name="vm">The page ViewModel to edit.</param>
    /// <returns>A <see cref="Task"/> that completes when the dialog closes.</returns>
    public static async Task OpenAsync(MainViewModel vm)
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
            await new ScopeManagerDialog(vm) { XamlRoot = root }.ShowAsync();
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
