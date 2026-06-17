using System.Diagnostics.CodeAnalysis;
using Microsoft.UI.Xaml;

namespace FindMyFiles.Services;

/// <summary>Seam over the app-exit step (<see cref="Application.Exit"/>) so
/// <see cref="ShellOps.RelaunchWith"/> is unit-testable and so the production
/// exit can marshal onto the UI thread. Mirrors <see cref="IProcessRunner"/>.</summary>
internal interface IAppExit
{
    /// <summary>Exit the application (process teardown).</summary>
    void Exit();
}

/// <summary>Production <see cref="IAppExit"/>: marshals
/// <c>Application.Current.Exit()</c> onto the cached UI <c>DispatcherQueue</c>
/// (<see cref="App.DispatcherQueue"/>). <c>Application.Exit()</c> is
/// UI-thread-affine, and <see cref="ShellOps.Relaunch"/> can be reached from a
/// thread-pool thread (the post-register pipe-probe continuation), where a direct
/// call throws RPC_E_WRONG_THREAD and leaves the old window orphaned beside the
/// new one — the setup-screen relaunch bug.</summary>
[ExcludeFromCodeCoverage] // UI-thread marshal wrapper; covered by docs/MANUAL_SMOKE.md + a real app run.
internal sealed class DispatcherAppExit : IAppExit
{
    /// <summary>Shared instance — the wrapper is stateless.</summary>
    internal static readonly DispatcherAppExit Instance = new();

    /// <inheritdoc/>
    public void Exit() => App.DispatcherQueue.TryEnqueue(() => Application.Current.Exit());
}
