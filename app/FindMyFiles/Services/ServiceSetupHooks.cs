using System.Diagnostics;
using System.Security.Principal;
using FindMyFiles.Engine;

namespace FindMyFiles.Services;

/// <summary>Complete native/process identity boundary used by
/// <see cref="ServiceSetup"/>. Tests replace it atomically; production retains
/// the image-lock lease until the elevated process exits.</summary>
/// <param name="OpenManager">Opens an owned SCM handle.</param>
/// <param name="AcquireExecutable">Verifies and pins the elevated helper image.</param>
/// <param name="StartProcess">Starts the elevated helper.</param>
/// <param name="IsProcessElevated">Checks the current process token.</param>
/// <param name="ReadCurrentUserSid">Reads the daily user's SID.</param>
/// <param name="ProbePipe">Probes the current service protocol pipe.</param>
/// <param name="Wait">Performs one bounded polling delay.</param>
internal sealed record ServiceSetupHooks(
    Func<uint, IServiceManagerHandle?> OpenManager,
    Func<string, TrustedServiceExecutable> AcquireExecutable,
    Func<ProcessStartInfo, IElevatedProcess?> StartProcess,
    Func<bool> IsProcessElevated,
    Func<string?> ReadCurrentUserSid,
    Func<string, bool> ProbePipe,
    Action<TimeSpan> Wait)
{
    /// <summary>The real SCM, image trust, process, token and pipe boundaries.</summary>
    internal static ServiceSetupHooks Production { get; } = new(
        ServiceSetupNative.OpenManager,
        static path =>
        {
            var lease = ServiceExecutableTrust.Acquire(path);
            return new TrustedServiceExecutable(lease.Path, lease);
        },
        ServiceSetupNative.StartProcess,
        static () =>
        {
            using var identity = WindowsIdentity.GetCurrent();
            return new WindowsPrincipal(identity)
                .IsInRole(WindowsBuiltInRole.Administrator);
        },
        static () =>
        {
            using var identity = WindowsIdentity.GetCurrent();
            return identity.User?.Value;
        },
        static pipeName => PipeEngineClient.Probe(pipeName, TimeSpan.FromMilliseconds(250)),
        Thread.Sleep);
}
