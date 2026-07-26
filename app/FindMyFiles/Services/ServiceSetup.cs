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
internal static partial class ServiceSetup
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
        const int ErrorServiceDoesNotExist = 1060;
        var scm = OpenSCManager(null, null, ScManagerConnect);
        if (scm == IntPtr.Zero)
        {
            return EngineServiceState.Unknown;
        }

        try
        {
            var svc = OpenService(scm, EngineContract.ServiceName, ServiceQueryStatus);
            if (svc == IntPtr.Zero)
            {
                return Marshal.GetLastWin32Error() == ErrorServiceDoesNotExist
                    ? EngineServiceState.NotInstalled
                    : EngineServiceState.Unknown;
            }

            try
            {
                if (!QueryServiceStatus(svc, out var status))
                {
                    return EngineServiceState.Unknown;
                }

                return MapServiceState(status.CurrentState);
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

    /// <summary>Map raw SERVICE_STATUS state. Only SERVICE_STOPPED proves the
    /// process and its writer lock are gone; every transition/paused state stays
    /// on the pipe-safe path.</summary>
    /// <param name="currentState">Win32 <c>dwCurrentState</c>.</param>
    /// <returns>The conservative app lifecycle state.</returns>
    internal static EngineServiceState MapServiceState(uint currentState) =>
        currentState == 1 ? EngineServiceState.Stopped : EngineServiceState.Running;

    /// <summary>Whether the installed stable service advertises the exact
    /// protocol marker generated from fmf-contract. The query needs only
    /// SERVICE_QUERY_CONFIG, which install grants to the authorized user.
    /// Missing/old descriptions fail closed to re-registration.</summary>
    /// <returns>True only for the current service protocol marker.</returns>
    public static bool IsInstalledServiceCompatible()
    {
        const uint ScManagerConnect = 0x0001;
        const uint ServiceQueryConfig = 0x0001;
        const uint ServiceConfigDescription = 1;
        const uint MaxDescriptionBytes = 4096;

        var scm = OpenSCManager(null, null, ScManagerConnect);
        if (scm == IntPtr.Zero)
        {
            return false;
        }

        try
        {
            var svc = OpenService(scm, EngineContract.ServiceName, ServiceQueryConfig);
            if (svc == IntPtr.Zero)
            {
                return false;
            }

            try
            {
                _ = QueryServiceConfig2(
                    svc,
                    ServiceConfigDescription,
                    IntPtr.Zero,
                    0,
                    out var bytesNeeded);
                if (bytesNeeded == 0 || bytesNeeded > MaxDescriptionBytes)
                {
                    return false;
                }

                var buffer = Marshal.AllocHGlobal((int)bytesNeeded);
                try
                {
                    if (!QueryServiceConfig2(
                        svc,
                        ServiceConfigDescription,
                        buffer,
                        bytesNeeded,
                        out _))
                    {
                        return false;
                    }

                    var description = Marshal.PtrToStructure<ServiceDescription>(buffer);
                    return IsServiceProtocolMarkerCompatible(
                        Marshal.PtrToStringUni(description.Description));
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

    /// <summary>Exact marker comparison, split out for deterministic tests.</summary>
    /// <param name="description">SCM service Description text.</param>
    /// <returns>True only for this build's generated marker.</returns>
    internal static bool IsServiceProtocolMarkerCompatible(string? description) =>
        string.Equals(
            description,
            EngineContract.ServiceProtocolMarker,
            StringComparison.Ordinal);

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

    /// <summary>Resolve the exact bundled companion next to the app.
    /// Test-seam builds additionally search the repository build tree; that
    /// ancestor walk is compiled out of stable artifacts so elevation can
    /// never select an unrelated developer binary.</summary>
    /// <param name="baseDir">Directory to start the search from (typically the app's bin dir).</param>
    /// <returns>Full path to fmf-service.exe, or null when it cannot be found.</returns>
    public static string? LocateServiceExe(string baseDir)
    {
        var bundled = Path.Combine(baseDir, "fmf-service.exe");
        if (File.Exists(bundled))
        {
            return Path.GetFullPath(bundled);
        }

#if FMF_TEST_SEAMS
        var dir = new DirectoryInfo(baseDir);
        for (var i = 0; i < 8 && dir is not null; i++, dir = dir.Parent)
        {
            var dev = Path.Combine(dir.FullName, "build", "engine", "release", "fmf-service.exe");
            if (File.Exists(dev))
            {
                return Path.GetFullPath(dev);
            }
        }
#endif

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
            using var trusted = ServiceExecutableTrust.Acquire(exe);
            using var p = Process.Start(new ProcessStartInfo
            {
                FileName = trusted.Path,
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
                TryTerminateTimedOutProcess(p, "elevated");
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
            FileLog.Warn("service-setup", "elevated service action failed", ex);
            return new ServiceActionResult(ServiceActionOutcome.Failed, -1);
        }
    }

    /// <summary>Starts the installed service WITHOUT elevation (on-demand
    /// lifecycle, ADR-0027): runs <c>fmf-service start</c> as the plain user,
    /// which succeeds because install granted this user SID SERVICE_START on the
    /// service object. No UAC, no window. Returns true once the start request was
    /// accepted; the caller must still verify that the current protocol pipe
    /// appears. False on any failure —
    /// including an older install that never granted the right — so the caller
    /// falls back to the setup screen, whose re-register migrates it. Blocking;
    /// call off the UI thread.</summary>
    /// <returns>True when the unelevated start request succeeded.</returns>
    public static bool TryStartUnelevated() => TryControlUnelevated("start");

    /// <summary>Stops the installed service without elevation using the
    /// install-time SERVICE_STOP grant. Used to unwind an obsolete service that
    /// started successfully but never exposed this build's protocol pipe.</summary>
    /// <returns>True when the stop completed successfully.</returns>
    public static bool TryStopUnelevated() => TryControlUnelevated("stop");

    /// <summary>Restarts the installed service without elevation using only the
    /// install-time SERVICE_STOP and SERVICE_START grants.</summary>
    /// <returns>True when stop + start completed successfully.</returns>
    public static bool TryRestartUnelevated() => TryControlUnelevated("restart");

    private static bool TryControlUnelevated(string verb)
    {
        var exe = LocateServiceExe(AppContext.BaseDirectory);
        if (exe is null)
        {
            FileLog.Event(
                "service-setup",
                "fmf-service.exe not found — cannot control service",
                ("verb", verb));
            return false;
        }

        try
        {
            using var p = Process.Start(new ProcessStartInfo
            {
                FileName = exe,
                Arguments = verb,
                UseShellExecute = false, // no runas: relies on the narrow service-object grant
                CreateNoWindow = true,
                RedirectStandardOutput = true,
                RedirectStandardError = true,
            });
            if (p is null)
            {
                return false;
            }

            if (!p.WaitForExit(15_000))
            {
                FileLog.Event(
                    "service-setup",
                    "unelevated service control timed out",
                    ("verb", verb));
                TryTerminateTimedOutProcess(p, "unelevated");
                return false;
            }

            // start/stop are idempotent. Non-zero means the SCM refused (an old
            // DACL, missing service, etc.); the caller falls back to setup.
            FileLog.Event(
                "service-setup",
                "unelevated service control completed",
                ("verb", verb),
                ("exit_code", p.ExitCode));
            return p.ExitCode == 0;
        }
        catch (Exception ex)
        {
            FileLog.Warn("service-setup", $"unelevated service {verb} failed", ex);
            return false;
        }
    }

    /// <summary>
    /// A timed-out lifecycle helper must not keep mutating SCM state after the
    /// UI has reported failure and allowed a retry. Best-effort termination is
    /// intentionally bounded; an elevated process handle may deny termination,
    /// in which case the failure is logged rather than hidden.
    /// </summary>
    private static void TryTerminateTimedOutProcess(Process process, string mode)
    {
        try
        {
            process.Kill(entireProcessTree: true);
            _ = process.WaitForExit(5_000);
            FileLog.Event(
                "service-setup",
                "timed-out service helper terminated",
                ("mode", mode),
                ("pid", process.Id));
        }
        catch (Exception ex)
        {
            FileLog.Warn(
                "service-setup",
                $"could not terminate timed-out {mode} service helper",
                ex);
        }
    }

    /// <summary>Wait for a just-started SCM service to expose the protocol pipe
    /// this app speaks. The service PID separates START_PENDING from RUNNING;
    /// once running, a short probe grace accommodates older compatible services
    /// that reported RUNNING just before creating their first pipe instance.
    /// A service on an obsolete versioned pipe never passes and is handed back
    /// to the caller for stop + setup recovery.</summary>
    /// <param name="pipeName">Current contract pipe name.</param>
    /// <returns>True only when a protocol-compatible Hello succeeds.</returns>
    public static bool WaitForCompatibleStartedService(string pipeName) =>
        PollForCompatibleStartedService(
            QueryServiceProcessId,
            () => PipeEngineClient.Probe(pipeName, TimeSpan.FromMilliseconds(250)),
            startPollAttempts: 100,
            compatibilityProbeAttempts: 5,
            () => Thread.Sleep(100));

    /// <summary>Purely injected polling core for
    /// <see cref="WaitForCompatibleStartedService"/>.</summary>
    /// <param name="servicePid">Returns a nonzero PID only once SCM is RUNNING.</param>
    /// <param name="probe">Attempts the current protocol Hello.</param>
    /// <param name="startPollAttempts">Maximum PID polls.</param>
    /// <param name="compatibilityProbeAttempts">Maximum Hello probes after RUNNING.</param>
    /// <param name="wait">Delay between attempts.</param>
    /// <returns>True only when a current-protocol Hello succeeds.</returns>
    internal static bool PollForCompatibleStartedService(
        Func<uint> servicePid,
        Func<bool> probe,
        int startPollAttempts,
        int compatibilityProbeAttempts,
        Action wait)
    {
        ArgumentOutOfRangeException.ThrowIfNegativeOrZero(startPollAttempts);
        ArgumentOutOfRangeException.ThrowIfNegativeOrZero(compatibilityProbeAttempts);

        for (var attempt = 0; attempt < startPollAttempts; attempt++)
        {
            if (servicePid() != 0)
            {
                for (var probeAttempt = 0;
                    probeAttempt < compatibilityProbeAttempts;
                    probeAttempt++)
                {
                    if (probe())
                    {
                        return true;
                    }

                    if (probeAttempt + 1 < compatibilityProbeAttempts)
                    {
                        wait();
                    }
                }

                return false;
            }

            if (attempt + 1 < startPollAttempts)
            {
                wait();
            }
        }

        return false;
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
            FileLog.Warn("service-setup", "current user SID query failed", ex);
            return null;
        }
    }

    /// <summary>A canonical decimal SID string: revision 1, a 48-bit identifier
    /// authority, and at most 15 32-bit sub-authorities. This guards the value
    /// going onto the fmf-service command line before it is interpolated; the
    /// elevated service additionally resolves it to a real user account.</summary>
    /// <param name="s">Candidate SID string to validate.</param>
    /// <returns>True when the value is a well-formed SID safe to pass on the command line.</returns>
    public static bool IsValidSid(string? s)
    {
        const ulong MaxIdentifierAuthority = 0x0000_FFFF_FFFF_FFFF;
        const int MaxSubAuthorities = 15;
        if (s is null || s.Length > 184)
        {
            return false;
        }

        var components = s.Split('-');
        if (components.Length is < 3 or > 3 + MaxSubAuthorities
            || !string.Equals(components[0], "S", StringComparison.Ordinal)
            || !string.Equals(components[1], "1", StringComparison.Ordinal)
            || !IsCanonicalDecimal(components[2])
            || !ulong.TryParse(components[2], out var authority)
            || authority > MaxIdentifierAuthority)
        {
            return false;
        }

        return components
            .Skip(3)
            .All(component =>
                IsCanonicalDecimal(component)
                && uint.TryParse(component, out _));
    }

    private static bool IsCanonicalDecimal(string component) =>
        component.Length > 0
        && (component.Length == 1 || component[0] != '0')
        && component.All(char.IsAsciiDigit);

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

    [LibraryImport("advapi32.dll", EntryPoint = "QueryServiceConfig2W", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool QueryServiceConfig2(
        IntPtr service,
        uint infoLevel,
        IntPtr buffer,
        uint bufSize,
        out uint bytesNeeded);

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

    [StructLayout(LayoutKind.Sequential)]
    private struct ServiceDescription
    {
        public IntPtr Description;
    }
}
