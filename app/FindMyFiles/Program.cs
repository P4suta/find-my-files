using System.Runtime.InteropServices;
using FindMyFiles.Services;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.Windows.AppLifecycle;

namespace FindMyFiles;

/// <summary>
/// Hand-written entry point (DISABLE_XAML_GENERATED_MAIN) that makes the app
/// single-instanced (ADR-0030): a second launch — e.g. from the Start menu while
/// the first instance is tray-resident — redirects its activation to the running
/// instance (which restores its window) and exits, instead of spawning a
/// duplicate process and a duplicate tray icon. Follows the WinAppSDK
/// single-instancing pattern (<c>AppInstance.FindOrRegisterForKey</c> +
/// <c>RedirectActivationTo</c>). The <see cref="App"/> ctor and <c>OnLaunched</c>
/// are unchanged; <c>Main</c> only wraps <c>Application.Start</c>.
/// </summary>
// View-shell entry point: imperative startup wiring, not unit-tested (ADR-0022).
[System.Diagnostics.CodeAnalysis.ExcludeFromCodeCoverage]
public static partial class Program
{
    private static IntPtr _redirectEvent;

    [STAThread]
    [System.Diagnostics.CodeAnalysis.SuppressMessage(
        "Performance",
        "CA1806:Do not ignore method results",
        Justification = "WinUI's App registers itself as Application.Current in its base ctor; the instance is intentionally not captured, matching the XAML-generated Main.")]
    private static int Main()
    {
        WinRT.ComWrappersSupport.InitializeComWrappers();

        if (DecideRedirection())
        {
            // A primary instance already owns the key; we redirected our
            // activation to it (restoring its window) and now exit.
            return 0;
        }

        Application.Start(_ =>
        {
            var context = new DispatcherQueueSynchronizationContext(
                DispatcherQueue.GetForCurrentThread());
            SynchronizationContext.SetSynchronizationContext(context);
            new App();
        });

        return 0;
    }

    /// <summary>Registers this process as the single-instance key owner, or — when
    /// one already exists — redirects this activation to it.</summary>
    /// <returns>True when this process should exit (it redirected).</returns>
    private static bool DecideRedirection()
    {
        var args = AppInstance.GetCurrent().GetActivatedEventArgs();
        var keyInstance = AppInstance.FindOrRegisterForKey("find-my-files");

        if (keyInstance.IsCurrent)
        {
            keyInstance.Activated += OnActivated;
            return false;
        }

        RedirectActivationTo(args, keyInstance);
        return true;
    }

    private static void OnActivated(object? sender, AppActivationArguments args)
    {
        // AppInstance.Activated fires on a background thread — marshal to the UI
        // thread before touching the window (CLAUDE.md UI rule).
        _ = App.DispatcherQueue?.TryEnqueue(App.ShowFromTray);
    }

    private static void RedirectActivationTo(AppActivationArguments args, AppInstance keyInstance)
    {
        _redirectEvent = CreateEvent(IntPtr.Zero, bManualReset: true, bInitialState: false, lpName: null);
        Task.Run(() =>
        {
            keyInstance.RedirectActivationToAsync(args).AsTask().Wait();
            _ = SetEvent(_redirectEvent);
        }).Forget("single-instance-redirect");

        // Pump COM while waiting so the cross-process redirect completes without
        // deadlocking this STA.
        const uint CwmoDefault = 0;
        const uint Infinite = 0xFFFFFFFF;
        _ = CoWaitForMultipleObjects(CwmoDefault, Infinite, 1, [_redirectEvent], out _);
    }

    [LibraryImport("kernel32.dll", EntryPoint = "CreateEventW",
        StringMarshalling = StringMarshalling.Utf16, SetLastError = true)]
    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    private static partial IntPtr CreateEvent(
        IntPtr lpEventAttributes,
        [MarshalAs(UnmanagedType.Bool)] bool bManualReset,
        [MarshalAs(UnmanagedType.Bool)] bool bInitialState,
        string? lpName);

    [LibraryImport("kernel32.dll", SetLastError = true)]
    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool SetEvent(IntPtr hEvent);

    [LibraryImport("ole32.dll")]
    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    private static partial uint CoWaitForMultipleObjects(
        uint dwFlags, uint dwMilliseconds, uint nHandles, IntPtr[] pHandles, out uint dwIndex);
}
