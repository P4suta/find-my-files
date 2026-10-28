using FindMyFiles.Engine;
using FindMyFiles.Services;
using FindMyFiles.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;

namespace FindMyFiles.Views;

/// <summary> Wiring only: the gear menu's "Manage service…"
/// dialog.
/// State and the service mutations live in <see cref="ServiceManagerViewModel"/>; the buttons fire-and-forget its async actions through the sanctioned <see cref="FindMyFiles.Services.TaskExtensions.Forget"/> funnel (AGENTS.md convention).
/// </summary>
[System.Diagnostics.CodeAnalysis.ExcludeFromCodeCoverage]
public sealed partial class ServiceManagerDialog : ContentDialog
{
    private static bool _open;

    /// <summary>ViewModel for service lifecycle operations.
    /// Each button fires this instance's async action via <c>Forget</c>.</summary>
    internal ServiceManagerViewModel VM { get; }

    /// <summary>Set when a soft restart closed this dialog, so the post-close transport re-check below is skipped — that restart already re-resolved the engine, and running a second one is pure churn.</summary>
    private bool _dismissedForReload;

    /// <summary>Creates the ViewModel and runs the initial state `Refresh`.
    /// The only public entry point is <see cref="OpenAsync"/>; direct constructor calls are not expected.</summary>
    public ServiceManagerDialog()
    {
        VM = new ServiceManagerViewModel();
        InitializeComponent();
        AutomationProperties.SetLabeledBy(PurgeConfirmation, SvcPurgeConfirmTitle);
        VM.Refresh();
    }

    /// <summary>The single entry point that opens the manager (the gear menu).
    /// Resolves a XamlRoot from the main window and guards against a second instance (ContentDialog allows only one open at a time).
    /// Named OpenAsync,
    /// not ShowAsync, to avoid hiding the inherited <see cref="ContentDialog.ShowAsync()"/>.</summary> <returns>A <see cref="Task"/> that completes when the dialog closes.</returns>
    internal static async Task OpenAsync()
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
            var dialog = new ServiceManagerDialog { XamlRoot = root };

            using (AppReload.TrackModal(dialog.DismissForReload))
            {
                await dialog.ShowAsync();
            }

            if (!dialog._dismissedForReload
                && !dialog.VM.FullUninstallCompleted
                && App.EngineClient.Kind == EngineClientKind.Service)
            {
                var state = ServiceSetup.QueryState();
                if (state == EngineServiceState.Stopped)
                {
                    App.SoftRestartIntoUnavailable();
                }
                else if (state == EngineServiceState.NotInstalled)
                {
                    App.SoftRestart();
                }
            }
        }
        catch (Exception ex)
        {
            FileLog.Error("service-ui", "service manager dialog failed", ex);
            Notifier.Post(NotifySeverity.Warning, Loc.Get("Svc_OpenFailed"), ex.Message);
        }
        finally
        {
            _open = false;
        }
    }

    /// <summary>Close this dialog because the page graph is being rebuilt (soft restart, ADR-0036) — usually by one of its own actions.</summary>
    private void DismissForReload()
    {
        _dismissedForReload = true;
        Hide();
    }

    private void Start_Click(object sender, RoutedEventArgs e) =>
        VM.StartAsync().Forget("service-ui");

    private void Stop_Click(object sender, RoutedEventArgs e) =>
        VM.StopAsync().Forget("service-ui");

    private void Restart_Click(object sender, RoutedEventArgs e) =>
        VM.RestartAsync().Forget("service-ui");

    private void Register_Click(object sender, RoutedEventArgs e) =>
        VM.RegisterAsync().Forget("service-ui");

    private void Uninstall_Click(object sender, RoutedEventArgs e) =>
        VM.UninstallAsync(purgeData: false).Forget("service-ui");

    private void RequestPurge_Click(object sender, RoutedEventArgs e)
    {
        VM.RequestPurgeConfirmation();
        if (VM.PurgeConfirmationVisible)
        {
            SvcPurgeConfirm.Focus(FocusState.Programmatic);
        }
    }

    private void ConfirmPurge_Click(object sender, RoutedEventArgs e) =>
        ConfirmPurgeAndRestoreFocusAsync().Forget("service-ui");

    private async Task ConfirmPurgeAndRestoreFocusAsync()
    {
        await VM.UninstallAsync(purgeData: true);
        if (!VM.FullUninstallCompleted)
        {
            SvcPurgeData.Focus(FocusState.Programmatic);
        }
    }

    private void CancelPurge_Click(object sender, RoutedEventArgs e)
    {
        VM.CancelPurgeConfirmation();
        SvcPurgeData.Focus(FocusState.Programmatic);
    }

    private void RestartApp_Click(object sender, RoutedEventArgs e) => VM.RestartApp();
}
