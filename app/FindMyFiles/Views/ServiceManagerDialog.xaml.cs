using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using FindMyFiles.Engine;
using FindMyFiles.Services;
using FindMyFiles.ViewModels;

namespace FindMyFiles.Views;

/// <summary>
/// Wiring only: the gear menu's「サービスの管理…」dialog. State and the
/// elevated mutations live in <see cref="ServiceManagerViewModel"/>; the
/// buttons fire-and-forget its async actions through the sanctioned
/// <see cref="FindMyFiles.Services.TaskExtensions.Forget"/> funnel (CLAUDE.md規約).
/// </summary>
public sealed partial class ServiceManagerDialog : ContentDialog
{
    private static bool _open;

    /// <summary>サービスの状態と昇格を要する操作(登録/削除/開始/停止/再起動)を担う
    /// ViewModel。各ボタンはこのインスタンスの async アクションを `Forget` で発火する。</summary>
    public ServiceManagerViewModel VM { get; }

    /// <summary>ViewModel を生成して初回の状態 `Refresh` を走らせる。公開入口は
    /// <see cref="OpenAsync"/> のみで、コンストラクタの直接呼び出しは想定しない。</summary>
    public ServiceManagerDialog()
    {
        VM = new ServiceManagerViewModel();
        InitializeComponent();
        VM.Refresh();
    }

    /// <summary>The single entry point that opens the manager (the gear menu).
    /// Resolves a XamlRoot from the main window and guards against a second
    /// instance (ContentDialog allows only one open at a time). Named OpenAsync,
    /// not ShowAsync, to avoid hiding the inherited
    /// <see cref="ContentDialog.ShowAsync()"/>.</summary>
    public static async Task OpenAsync()
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
            await new ServiceManagerDialog { XamlRoot = root }.ShowAsync();
            // If the service was uninstalled/stopped while this instance was
            // running on the pipe, its connection is dead and can't recover —
            // relaunch so the app re-resolves the engine and lands on the setup
            // screen (the mirror of register's relaunch). Register's own relaunch
            // already exited the process before this when the user re-registered.
            if (App.EngineClient is PipeEngineClient
                && ServiceSetup.QueryState() != EngineServiceState.Running)
            {
                ShellOps.Relaunch();
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

    private void Start_Click(object sender, RoutedEventArgs e) =>
        VM.StartAsync().Forget("service-ui");

    private void Stop_Click(object sender, RoutedEventArgs e) =>
        VM.StopAsync().Forget("service-ui");

    private void Restart_Click(object sender, RoutedEventArgs e) =>
        VM.RestartAsync().Forget("service-ui");

    private void Register_Click(object sender, RoutedEventArgs e) =>
        VM.RegisterAsync().Forget("service-ui");

    private void Uninstall_Click(object sender, RoutedEventArgs e) =>
        VM.UninstallAsync().Forget("service-ui");

    private void RestartApp_Click(object sender, RoutedEventArgs e) => VM.RestartApp();
}
