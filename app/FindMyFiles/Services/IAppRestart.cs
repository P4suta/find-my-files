using System.Diagnostics.CodeAnalysis;
using Microsoft.Windows.AppLifecycle;

namespace FindMyFiles.Services;

/// <summary>Seam over a true process restart so <see cref="ShellOps.Relaunch"/>
/// is unit-testable. Mirrors <see cref="IProcessRunner"/>. Used only by the
/// language switch, where a process-global WinRT setting
/// (<c>PrimaryLanguageOverride</c>, applied in the <see cref="App"/> ctor) and the
/// window chrome must be rebuilt — an in-process <see cref="AppReload"/> only
/// rebuilds the page body. Every other "restart" reason is in-process (ADR-0036).</summary>
internal interface IAppRestart
{
    /// <summary>Terminate and restart this app, passing <paramref name="arguments"/>
    /// to the fresh instance.</summary>
    /// <param name="arguments">Command line handed to the restarted instance.</param>
    void Restart(string arguments);
}

/// <summary>Production <see cref="IAppRestart"/>: the Windows App SDK
/// <see cref="AppInstance.Restart(string)"/>. Unlike a raw
/// <c>Process.Start</c> + <c>Application.Exit</c>, it fully terminates this
/// process before the fresh one registers, so single-instancing
/// (<c>Program.DecideRedirection</c>) lets the new instance become primary
/// instead of redirecting its activation back to this dying one (ADR-0036).
/// A success never returns (the process is gone); a non-success return is a
/// genuine failure, surfaced as an exception so <see cref="ShellOps.Run"/>
/// notifies the user rather than going silent.</summary>
[ExcludeFromCodeCoverage] // thin OS-API wrapper; covered by docs/MANUAL_SMOKE.md + a real app run.
internal sealed class RealAppRestart : IAppRestart
{
    /// <summary>Shared instance — the wrapper is stateless.</summary>
    internal static readonly RealAppRestart Instance = new();

    /// <inheritdoc/>
    public void Restart(string arguments)
    {
        var reason = AppInstance.Restart(arguments);

        // Only reached when the restart did not happen (success terminates us).
        throw new InvalidOperationException($"AppInstance.Restart failed: {reason}");
    }
}
