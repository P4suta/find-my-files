using System.Runtime.InteropServices;
using FindMyFiles.Services;
using Microsoft.Win32.SafeHandles;

namespace FindMyFiles.Engine;

/// <summary>
/// Client-side fake-server defense (SECURITY.md Threat 4, pipe-name squatting):
/// the process at the far end of the default pipe must be the SCM-registered
/// fmf-engine service. Comparing the pipe's server PID to the service's PID
/// works from an unelevated client — reading a SYSTEM process's token does
/// not (ACCESS_DENIED), and the session manager won't expose a session-0
/// SYSTEM process's identity to an unelevated caller either. A squatter never
/// matches: registering the service needs admin. Fails closed.
/// </summary>
internal static partial class PipeServerIdentity
{
    /// <summary>True only when the pipe's server is the running fmf-engine
    /// service process.</summary>
    /// <param name="pipe">The connected default-pipe handle to inspect.</param>
    /// <returns>True when the pipe's server process is the fmf-engine service; otherwise false.</returns>
    internal static bool IsServerTrusted(SafePipeHandle pipe) =>
        GetNamedPipeServerProcessId(pipe, out var serverPid) && IsServiceProcess(serverPid);

    /// <summary>True when <paramref name="pid"/> is the running fmf-engine
    /// service process. Split out so unit tests can exercise it without a
    /// live pipe.</summary>
    /// <param name="pid">The process id to compare against the live service's pid.</param>
    /// <returns>True when the service is running and its pid equals <paramref name="pid"/>; otherwise false.</returns>
    internal static bool IsServiceProcess(uint pid)
    {
        var servicePid = ServiceSetup.QueryServiceProcessId();
        return servicePid != 0 && pid == servicePid;
    }

    [LibraryImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool GetNamedPipeServerProcessId(
        SafePipeHandle pipe, out uint serverProcessId);
}
