using System.Diagnostics;
using System.Runtime.InteropServices;
using FindMyFiles.Engine;

namespace FindMyFiles.Services;

/// <summary>
/// In-app service setup — the GUI half ADR-0016 left to a terminal: detects
/// the fmf-engine SCM registration (read-only, works unelevated) and drives
/// fmf-service.exe install/start so the one-time elevation never needs
/// PowerShell. Mutations are strictly user-initiated (the notification
/// button); install is idempotent on the service side.
/// </summary>
public static partial class ServiceSetup
{
    /// <summary>True when *this* process is already running with an
    /// Administrator token — the in-proc engine path needs it, and when set the
    /// in-app install/start verbs can skip their own UAC prompt.</summary>
    /// <returns>True when the current process token is in the Administrators role.</returns>
    public static bool IsProcessElevated()
    {
        using var identity = System.Security.Principal.WindowsIdentity.GetCurrent();
        return new System.Security.Principal.WindowsPrincipal(identity)
            .IsInRole(System.Security.Principal.WindowsBuiltInRole.Administrator);
    }

    /// <summary>Read-only SCM query for <see cref="EngineContract.ServiceName"/>.</summary>
    /// <returns>The service's install/run state for the offer logic.</returns>
    public static EngineServiceState QueryState()
    {
        const uint ScManagerConnect = 0x0001;
        const uint ServiceQueryStatus = 0x0004;
        var scm = OpenSCManager(null, null, ScManagerConnect);
        if (scm == IntPtr.Zero)
        {
            return EngineServiceState.NotInstalled;
        }

        try
        {
            var svc = OpenService(scm, EngineContract.ServiceName, ServiceQueryStatus);
            if (svc == IntPtr.Zero)
            {
                return EngineServiceState.NotInstalled; // ERROR_SERVICE_DOES_NOT_EXIST
            }

            try
            {
                if (!QueryServiceStatus(svc, out var status))
                {
                    return EngineServiceState.Stopped;
                }

                // 2=START_PENDING 4=RUNNING 5=CONTINUE_PENDING — anything on
                // its way up counts as running for the offer logic.
                return status.CurrentState is 2 or 4 or 5
                    ? EngineServiceState.Running
                    : EngineServiceState.Stopped;
            }
            finally
            {
                CloseServiceHandle(svc);
            }
        }
        finally
        {
            CloseServiceHandle(scm);
        }
    }

    /// <summary>PID of the running fmf-engine service process, or 0 when it is
    /// not installed/running. The client-side fake-server check (threat 4)
    /// compares this to the pipe's server PID — an unelevated client can read
    /// it (unlike a SYSTEM process's token), and a squatter never matches
    /// because registering the service needs admin.</summary>
    /// <returns>The running service's process id, or 0 when not installed/running.</returns>
    public static uint QueryServiceProcessId()
    {
        const uint ScManagerConnect = 0x0001;
        const uint ServiceQueryStatus = 0x0004;
        const int ScStatusProcessInfo = 0;
        const uint ServiceRunning = 4;
        var scm = OpenSCManager(null, null, ScManagerConnect);
        if (scm == IntPtr.Zero)
        {
            return 0;
        }

        try
        {
            var svc = OpenService(scm, EngineContract.ServiceName, ServiceQueryStatus);
            if (svc == IntPtr.Zero)
            {
                return 0;
            }

            try
            {
                var size = (uint)Marshal.SizeOf<ServiceStatusProcess>();
                var buffer = Marshal.AllocHGlobal((int)size);
                try
                {
                    if (!QueryServiceStatusEx(svc, ScStatusProcessInfo, buffer, size, out _))
                    {
                        return 0;
                    }

                    var status = Marshal.PtrToStructure<ServiceStatusProcess>(buffer);

                    // dwProcessId is only meaningful while RUNNING.
                    return status.CurrentState == ServiceRunning ? status.ProcessId : 0;
                }
                finally
                {
                    Marshal.FreeHGlobal(buffer);
                }
            }
            finally
            {
                CloseServiceHandle(svc);
            }
        }
        finally
        {
            CloseServiceHandle(scm);
        }
    }

    /// <summary>fmf-service.exe next to the app (the dist bundle) or in the
    /// dev tree (build\engine\release, walking up from the bin dir).</summary>
    /// <param name="baseDir">Directory to start the search from (typically the app's bin dir).</param>
    /// <returns>Full path to fmf-service.exe, or null when it cannot be found.</returns>
    public static string? LocateServiceExe(string baseDir)
    {
        var bundled = Path.Combine(baseDir, "fmf-service.exe");
        if (File.Exists(bundled))
        {
            return bundled;
        }

        var dir = new DirectoryInfo(baseDir);
        for (var i = 0; i < 8 && dir is not null; i++, dir = dir.Parent)
        {
            var dev = Path.Combine(dir.FullName, "build", "engine", "release", "fmf-service.exe");
            if (File.Exists(dev))
            {
                return dev;
            }
        }

        return null;
    }

    /// <summary>Run one fmf-service lifecycle verb elevated via a per-action
    /// UAC prompt (Verb=runas) — the in-app service manager, where the app
    /// itself stays asInvoker. Output can't be captured under ShellExecute,
    /// so the verdict is the exit code; a declined prompt (ERROR_CANCELLED
    /// 1223) is reported distinctly. <paramref name="args"/> is built from
    /// fixed verbs plus SID-validated flags, never raw user text. Blocking —
    /// call off the UI thread.</summary>
    /// <param name="exe">Path to fmf-service.exe to launch elevated.</param>
    /// <param name="args">Service verb plus SID-validated flags to pass on the command line.</param>
    /// <returns>The classified outcome and raw exit code of the elevated action.</returns>
    public static ServiceActionResult RunElevated(string exe, string args)
    {
        try
        {
            using var p = Process.Start(new ProcessStartInfo
            {
                FileName = exe,
                Arguments = args,
                UseShellExecute = true, // required for the runas verb
                Verb = "runas", // elevate just this action; the app stays asInvoker

                // A console exe under ShellExecute ignores CreateNoWindow; hide
                // the window so the verb doesn't flash a console.
                WindowStyle = ProcessWindowStyle.Hidden,
            });
            if (p is null)
            {
                return new ServiceActionResult(ServiceActionOutcome.Failed, -1);
            }

            if (!p.WaitForExit(60_000))
            {
                return new ServiceActionResult(ServiceActionOutcome.Failed, -1);
            }

            return new ServiceActionResult(
                p.ExitCode == 0 ? ServiceActionOutcome.Ok : ServiceActionOutcome.Failed,
                p.ExitCode);
        }
        catch (System.ComponentModel.Win32Exception ex) when (ex.NativeErrorCode == 1223)
        {
            // ERROR_CANCELLED — the user dismissed the UAC prompt.
            return new ServiceActionResult(ServiceActionOutcome.Cancelled, -1);
        }
        catch (Exception ex)
        {
            FileLog.Warn("service-setup", $"elevated `{args}` failed: {ex.Message}");
            return new ServiceActionResult(ServiceActionOutcome.Failed, -1);
        }
    }

    /// <summary>The current user's SID string, forwarded to
    /// `fmf-service install --owner-sid` so OTS elevation (a *different*
    /// admin account) does not lock this user out of the pipe (threat 1).
    /// Null when unavailable — install then authorizes only the elevated
    /// account.</summary>
    /// <returns>The current user's SID string, or null when it cannot be read.</returns>
    public static string? CurrentUserSid()
    {
        try
        {
            using var id = System.Security.Principal.WindowsIdentity.GetCurrent();
            return id.User?.Value;
        }
        catch (Exception ex)
        {
            FileLog.Warn("service-setup", $"current user SID query failed: {ex.Message}");
            return null;
        }
    }

    /// <summary>A well-formed SID string (S-1-… of digits and hyphens) —
    /// guards the value going onto the fmf-service command line against
    /// argument injection before it is interpolated.</summary>
    /// <param name="s">Candidate SID string to validate.</param>
    /// <returns>True when the value is a well-formed SID safe to pass on the command line.</returns>
    public static bool IsValidSid(string? s) =>
        s is not null
        && s.StartsWith("S-1-", StringComparison.Ordinal)
        && s.All(c => char.IsAsciiLetterOrDigit(c) || c == '-');

    [LibraryImport("advapi32.dll", EntryPoint = "OpenSCManagerW",
        StringMarshalling = StringMarshalling.Utf16, SetLastError = true)]
    private static partial IntPtr OpenSCManager(string? machine, string? database, uint access);

    [LibraryImport("advapi32.dll", EntryPoint = "OpenServiceW",
        StringMarshalling = StringMarshalling.Utf16, SetLastError = true)]
    private static partial IntPtr OpenService(IntPtr scm, string name, uint access);

    [LibraryImport("advapi32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool QueryServiceStatus(IntPtr service, out ServiceStatus status);

    [LibraryImport("advapi32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool QueryServiceStatusEx(
        IntPtr service, int infoLevel, IntPtr buffer, uint bufSize, out uint bytesNeeded);

    [LibraryImport("advapi32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool CloseServiceHandle(IntPtr handle);

    [StructLayout(LayoutKind.Sequential)]
    private struct ServiceStatus
    {
        public uint ServiceType;
        public uint CurrentState;
        public uint ControlsAccepted;
        public uint Win32ExitCode;
        public uint ServiceSpecificExitCode;
        public uint CheckPoint;
        public uint WaitHint;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct ServiceStatusProcess
    {
        public uint ServiceType;
        public uint CurrentState;
        public uint ControlsAccepted;
        public uint Win32ExitCode;
        public uint ServiceSpecificExitCode;
        public uint CheckPoint;
        public uint WaitHint;
        public uint ProcessId;
        public uint ServiceFlags;
    }
}
