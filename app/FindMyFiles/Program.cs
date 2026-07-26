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
internal static partial class Program
{
    [STAThread]
    [System.Diagnostics.CodeAnalysis.SuppressMessage(
        "Performance",
        "CA1806:Do not ignore method results",
        Justification = "WinUI's App registers itself as Application.Current in its base ctor; the instance is intentionally not captured, matching the XAML-generated Main.")]
    private static int Main()
    {
        WinRT.ComWrappersSupport.InitializeComWrappers();
        LogSetup.Init();

        try
        {
            if (DecideRedirection())
            {
                // A primary instance already owns the key; we redirected our
                // activation to it (restoring its window) and now exit.
                LogSetup.Shutdown();
                return 0;
            }
        }
        catch (Exception ex)
        {
            FileLog.Error("startup", "single-instance activation redirect failed", ex);
            LogSetup.Shutdown();
            return 1;
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
        // thread before touching the window (AGENTS.md UI rule).
        _ = App.DispatcherQueue?.TryEnqueue(App.ShowFromTray);
    }

    private static void RedirectActivationTo(AppActivationArguments args, AppInstance keyInstance)
    {
        var completed = new EventWaitHandle(
            initialState: false,
            EventResetMode.ManualReset);
        var redirectTask = Task.Run(async () =>
        {
            try
            {
                await keyInstance.RedirectActivationToAsync(args).AsTask().ConfigureAwait(false);
            }
            finally
            {
                // Wake the COM-pumping primary thread on success and failure.
                // The task exception is rethrown below after the wait.
                _ = completed.Set();
            }
        });

        // Pump COM while waiting so the cross-process redirect completes without
        // deadlocking this STA. Bound the wait so a broken AppLifecycle broker
        // cannot leave a second launch stuck forever.
        const uint CwmoDefault = 0;
        const uint RedirectTimeoutMs = 30_000;
        var hr = CoWaitForMultipleObjects(
            CwmoDefault,
            RedirectTimeoutMs,
            1,
            [completed.SafeWaitHandle.DangerousGetHandle()],
            out _);
        if (hr != 0)
        {
            // RedirectActivationToAsync has no cancellation token. Keep the
            // event alive until the in-flight operation finishes.
            redirectTask.ContinueWith(
                static (task, state) =>
                {
                    _ = task.Exception;
                    ((EventWaitHandle)state!).Dispose();
                },
                completed,
                CancellationToken.None,
                TaskContinuationOptions.ExecuteSynchronously,
                TaskScheduler.Default).Forget("single-instance-redirect-cleanup");
            throw Marshal.GetExceptionForHR(unchecked((int)hr))
                ?? new InvalidOperationException(
                    $"activation redirect wait failed (HRESULT 0x{hr:X8})",
                    innerException: null);
        }

        try
        {
            redirectTask.GetAwaiter().GetResult();
        }
        finally
        {
            completed.Dispose();
        }
    }

    [LibraryImport("ole32.dll")]
    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    private static partial uint CoWaitForMultipleObjects(
        uint dwFlags, uint dwMilliseconds, uint nHandles, IntPtr[] pHandles, out uint dwIndex);
}
