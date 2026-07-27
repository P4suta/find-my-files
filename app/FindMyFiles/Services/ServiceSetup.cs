using System.Diagnostics;
using System.Runtime.InteropServices;
using FindMyFiles.Engine;

namespace FindMyFiles.Services;

/// <summary>
/// In-app service setup — the GUI half ADR-0016 left to a terminal: detects
/// the fmf-engine SCM registration, controls an installed service directly
/// through its narrowly delegated SCM rights, and uses the pinned
/// fmf-service.exe only for one-time elevated setup/removal. Mutations are
/// strictly user-initiated; install is idempotent on the service side.
/// </summary>
internal static partial class ServiceSetup
{
    private const uint ServiceStopped = 1;
    private const uint ServiceStartPending = 2;
    private const uint ServiceStopPending = 3;
    private const uint ServiceRunning = 4;
    private const uint ServiceContinuePending = 5;
    private const uint ServicePausePending = 6;
    private const uint ServicePaused = 7;
    private const int ErrorServiceAlreadyRunning = 1056;
    private const int ErrorServiceCannotAcceptControl = 1061;
    private const int ErrorServiceNotActive = 1062;
    private const int ServiceControlPollAttempts = 150;
    private static readonly TimeSpan ServiceControlPollInterval =
        TimeSpan.FromMilliseconds(100);

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
    /// lifecycle, ADR-0027) by calling SCM directly with only
    /// SERVICE_START|SERVICE_QUERY_STATUS. No helper process, UAC, executable
    /// lookup, or command-line surface is involved. Returns true once SCM
    /// reaches RUNNING; the caller must still verify that the current protocol
    /// pipe appears. False on any failure —
    /// including an older install that never granted the right — so the caller
    /// falls back to the setup screen, whose re-register migrates it. Blocking;
    /// call off the UI thread.</summary>
    /// <returns>True when the unelevated start request succeeded.</returns>
    public static bool TryStartUnelevated() =>
        TryControlUnelevated(ScmControlVerb.Start);

    /// <summary>Stops the installed service directly through SCM without
    /// elevation using only the install-time SERVICE_STOP|SERVICE_QUERY_STATUS
    /// grant. Used to unwind an obsolete service that started successfully but
    /// never exposed this build's protocol pipe.</summary>
    /// <returns>True when the stop completed successfully.</returns>
    public static bool TryStopUnelevated() =>
        TryControlUnelevated(ScmControlVerb.Stop);

    /// <summary>Restarts the installed service directly through SCM without
    /// elevation using only the install-time START/STOP/QUERY grants.</summary>
    /// <returns>True when stop + start completed successfully.</returns>
    public static bool TryRestartUnelevated() =>
        TryControlUnelevated(ScmControlVerb.Restart);

    private static bool TryControlUnelevated(ScmControlVerb verb)
    {
        try
        {
            return TryControlUnelevatedCore(verb);
        }
        catch (Exception ex)
        {
            FileLog.Warn(
                "service-setup",
                $"unelevated SCM {verb} failed",
                ex);
            return false;
        }
    }

    private static bool TryControlUnelevatedCore(ScmControlVerb verb)
    {
        const uint ScManagerConnect = 0x0001;
        const uint ServiceQueryStatus = 0x0004;
        const uint ServiceStart = 0x0010;
        const uint ServiceStop = 0x0020;

        var scm = OpenSCManager(null, null, ScManagerConnect);
        if (scm == IntPtr.Zero)
        {
            FileLog.Event(
                "service-setup",
                "could not open SCM for unelevated service control",
                ("verb", verb.ToString()),
                ("win32", Marshal.GetLastWin32Error()));
            return false;
        }

        try
        {
            var verbAccess = verb switch
            {
                ScmControlVerb.Start => ServiceStart,
                ScmControlVerb.Stop => ServiceStop,
                ScmControlVerb.Restart => ServiceStart | ServiceStop,
                _ => 0u,
            };
            var access = ServiceQueryStatus | verbAccess;
            var service = OpenService(scm, EngineContract.ServiceName, access);
            if (service == IntPtr.Zero)
            {
                FileLog.Event(
                    "service-setup",
                    "could not open service for unelevated control",
                    ("verb", verb.ToString()),
                    ("win32", Marshal.GetLastWin32Error()));
                return false;
            }

            try
            {
                var success = DriveServiceControl(
                    verb,
                    () => QueryServiceStatus(service, out var status)
                        ? status.CurrentState
                        : null,
                    () => StartServiceNative(service, 0, IntPtr.Zero)
                        ? 0
                        : Marshal.GetLastWin32Error(),
                    () => ControlServiceNative(service, 1, out _)
                        ? 0
                        : Marshal.GetLastWin32Error(),
                    ServiceControlPollAttempts,
                    () => Thread.Sleep(ServiceControlPollInterval));
                FileLog.Event(
                    "service-setup",
                    "unelevated SCM control completed",
                    ("verb", verb.ToString()),
                    ("success", success));
                return success;
            }
            finally
            {
                CloseServiceHandle(service);
            }
        }
        finally
        {
            CloseServiceHandle(scm);
        }
    }

    internal enum ScmControlVerb
    {
        Start,
        Stop,
        Restart,
    }

    /// <summary>Deterministic bounded SCM lifecycle state machine. Native
    /// wrappers supply states and Win32 result codes; one shared poll budget
    /// covers the whole restart, preventing stop+start from each consuming a
    /// full timeout.</summary>
    /// <param name="verb">Target lifecycle operation.</param>
    /// <param name="queryState">Returns the latest SCM state, or null on query failure.</param>
    /// <param name="start">Issues StartService and returns zero or a Win32 error.</param>
    /// <param name="stop">Issues SERVICE_CONTROL_STOP and returns zero or a Win32 error.</param>
    /// <param name="maxPollAttempts">Shared maximum status queries for the whole operation.</param>
    /// <param name="wait">Bounded delay between status queries.</param>
    /// <returns>True only when the requested terminal state is observed.</returns>
    internal static bool DriveServiceControl(
        ScmControlVerb verb,
        Func<uint?> queryState,
        Func<int> start,
        Func<int> stop,
        int maxPollAttempts,
        Action wait)
    {
        ArgumentOutOfRangeException.ThrowIfNegativeOrZero(maxPollAttempts);
        var remaining = maxPollAttempts;
        return verb switch
        {
            ScmControlVerb.Start =>
                DriveToRunning(queryState, start, wait, ref remaining),
            ScmControlVerb.Stop =>
                DriveToStopped(queryState, stop, wait, ref remaining),
            ScmControlVerb.Restart =>
                DriveToStopped(queryState, stop, wait, ref remaining)
                && DriveToRunning(queryState, start, wait, ref remaining),
            _ => false,
        };
    }

    private static bool DriveToRunning(
        Func<uint?> queryState,
        Func<int> start,
        Action wait,
        ref int remaining)
    {
        var startIssued = false;
        while (TryReadState(queryState, ref remaining, out var state))
        {
            switch (state)
            {
                case ServiceRunning:
                    return true;
                case ServiceStopped when startIssued:
                    return false; // accepted start terminated before RUNNING
                case ServiceStopped:
                {
                    var error = start();
                    if (error is not 0 and not ErrorServiceAlreadyRunning)
                    {
                        return false;
                    }

                    startIssued = true;
                    break;
                }

                case ServiceStartPending:
                case ServiceStopPending:
                    break;
                default:
                    return false; // paused/malformed state is not a usable pipe host
            }

            if (!WaitForNextPoll(wait, remaining))
            {
                return false;
            }
        }

        return false;
    }

    private static bool DriveToStopped(
        Func<uint?> queryState,
        Func<int> stop,
        Action wait,
        ref int remaining)
    {
        var stopIssued = false;
        while (TryReadState(queryState, ref remaining, out var state))
        {
            switch (state)
            {
                case ServiceStopped:
                    return true;
                case ServiceStopPending:
                case ServiceStartPending:
                    break;
                case ServiceRunning:
                case ServiceContinuePending:
                case ServicePausePending:
                case ServicePaused:
                    if (!stopIssued)
                    {
                        var error = stop();
                        if (error == ErrorServiceCannotAcceptControl)
                        {
                            break; // transition raced us; query and retry
                        }

                        if (error is not 0 and not ErrorServiceNotActive)
                        {
                            return false;
                        }

                        stopIssued = true;
                    }

                    break;
                default:
                    return false;
            }

            if (!WaitForNextPoll(wait, remaining))
            {
                return false;
            }
        }

        return false;
    }

    private static bool TryReadState(
        Func<uint?> queryState,
        ref int remaining,
        out uint state)
    {
        state = 0;
        if (remaining <= 0)
        {
            return false;
        }

        remaining--;
        var current = queryState();
        if (current is null)
        {
            return false;
        }

        state = current.Value;
        return true;
    }

    private static bool WaitForNextPoll(Action wait, int remaining)
    {
        if (remaining <= 0)
        {
            return false;
        }

        wait();
        return true;
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
    /// Null when unavailable. Callers must fail closed rather than starting an
    /// owner-less setup: under over-the-shoulder elevation the administrator is
    /// a different identity and the daily user would be locked out.</summary>
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

    /// <summary>Build the one supported setup command line for the current daily
    /// user. There is deliberately no owner-less fallback: failing to bind the
    /// unelevated identity before UAC would install a service that the caller
    /// cannot connect to or control.</summary>
    /// <param name="arguments">The injection-safe setup arguments on success.</param>
    /// <returns>True only when the current user's SID can be captured and validated.</returns>
    public static bool TryCreateSetupArguments(out string arguments) =>
        TryCreateSetupArguments(CurrentUserSid(), out arguments);

    /// <summary>Pure setup-argument builder used by the fail-closed boundary and
    /// its tests.</summary>
    /// <param name="sid">Candidate daily-user SID.</param>
    /// <param name="arguments">The exact setup arguments, or empty on failure.</param>
    /// <returns>True only for a command-line-safe canonical SID.</returns>
    internal static bool TryCreateSetupArguments(string? sid, out string arguments)
    {
        if (!IsValidSid(sid))
        {
            arguments = string.Empty;
            return false;
        }

        arguments = $"setup --owner-sid={sid}";
        return true;
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

    [LibraryImport("advapi32.dll", EntryPoint = "StartServiceW", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool StartServiceNative(
        IntPtr service,
        uint argumentCount,
        IntPtr argumentVectors);

    [LibraryImport("advapi32.dll", EntryPoint = "ControlService", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool ControlServiceNative(
        IntPtr service,
        uint control,
        out ServiceStatus status);

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
