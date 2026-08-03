using FindMyFiles.Services;

namespace FindMyFiles.Engine;

/// <summary>Injectable process/SCM/transport boundary used by the startup
/// resolver. Production supplies the real Windows operations; unit tests supply
/// deterministic callbacks so every transport outcome is behaviorally covered.</summary>
/// <param name="Probe">Probe one pipe name for a compatible service.</param>
/// <param name="QueryState">Read the installed service state.</param>
/// <param name="ServiceCompatible">Check the installed protocol marker.</param>
/// <param name="TryStart">Try to start the service without elevation.</param>
/// <param name="IsElevated">Report whether the current process is elevated.</param>
/// <param name="OpenPipe">Construct a pipe engine for one pipe name.</param>
/// <param name="OpenInProc">Construct the explicit in-process engine.</param>
internal sealed record EngineClientFactoryHooks(
    Func<string, bool> Probe,
    Func<EngineServiceState> QueryState,
    Func<bool> ServiceCompatible,
    Func<bool> TryStart,
    Func<bool> IsElevated,
    Func<string, IEngineClient> OpenPipe,
    Func<IEngineClient> OpenInProc);
