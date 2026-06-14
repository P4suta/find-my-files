using System.Diagnostics;

namespace FindMyFiles.Services;

/// <summary>Seam over <see cref="Process.Start(ProcessStartInfo)"/> so shell
/// launches (open, …) can be driven by a fake in tests — the launch behaviour,
/// not just the <see cref="ProcessStartInfo"/> arguments, gets covered. Mirrors
/// the app's other injectable boundaries (<c>IEngineClient</c>, <c>IDispatcher</c>).</summary>
internal interface IProcessRunner
{
    /// <summary>Start the process described by <paramref name="psi"/>.</summary>
    /// <param name="psi">Fully built start info.</param>
    void Start(ProcessStartInfo psi);
}

/// <summary>Production <see cref="IProcessRunner"/>: starts the real process and
/// releases the returned handle (the launched process keeps running).</summary>
internal sealed class RealProcessRunner : IProcessRunner
{
    /// <summary>Shared instance — <see cref="Process.Start(ProcessStartInfo)"/> is stateless.</summary>
    internal static readonly RealProcessRunner Instance = new();

    /// <inheritdoc/>
    public void Start(ProcessStartInfo psi) => Process.Start(psi)?.Dispose();
}
