using System.Diagnostics;
using System.Runtime.InteropServices;
using FindMyFiles.Engine;

namespace FindMyFiles.Services;

public enum EngineServiceState
{
    NotInstalled,
    Stopped,
    Running,
}

/// <summary>
/// In-app service setup — the GUI half ADR-0016 left to a terminal: detects
/// the fmf-engine SCM registration (read-only, works unelevated) and drives
/// fmf-service.exe install/start so the one-time elevation never needs
/// PowerShell. Mutations are strictly user-initiated (the notification
/// button); install is idempotent on the service side.
/// </summary>
public static partial class ServiceSetup
{
    public static bool IsProcessElevated()
    {
        using var identity = System.Security.Principal.WindowsIdentity.GetCurrent();
        return new System.Security.Principal.WindowsPrincipal(identity)
            .IsInRole(System.Security.Principal.WindowsBuiltInRole.Administrator);
    }

    /// <summary>Read-only SCM query for <see cref="EngineContract.ServiceName"/>.</summary>
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

    /// <summary>fmf-service.exe next to the app (the dist bundle) or in the
    /// dev tree (engine\target\release, walking up from the bin dir).</summary>
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
            var dev = Path.Combine(dir.FullName, "engine", "target", "release", "fmf-service.exe");
            if (File.Exists(dev))
            {
                return dev;
            }
        }
        return null;
    }

    /// <summary>install (idempotent server-side) then start. Blocking — run
    /// off the UI thread. The transcript feeds the failure notification and
    /// app.log, so a failure always says why.</summary>
    public static (bool Ok, string Transcript) InstallAndStart(string serviceExe)
    {
        var log = new System.Text.StringBuilder();
        foreach (var verb in new[] { "install", "start" })
        {
            var (code, output) = RunTool(serviceExe, verb);
            log.AppendLine($"fmf-service {verb} (exit {code})");
            log.AppendLine(output.Trim());
            if (code != 0)
            {
                return (false, log.ToString());
            }
        }
        return (true, log.ToString());
    }

    private static (int ExitCode, string Output) RunTool(string exe, string args)
    {
        try
        {
            using var p = Process.Start(new ProcessStartInfo
            {
                FileName = exe,
                Arguments = args,
                UseShellExecute = false,
                CreateNoWindow = true,
                RedirectStandardOutput = true,
                RedirectStandardError = true,
            })!;
            var stdout = p.StandardOutput.ReadToEnd();
            var stderr = p.StandardError.ReadToEnd();
            if (!p.WaitForExit(60_000))
            {
                try
                {
                    p.Kill();
                }
                catch
                {
                    // already gone — the timeout verdict stands either way
                }
                return (-1, "timed out after 60s");
            }
            return (p.ExitCode, stdout + stderr);
        }
        catch (Exception ex)
        {
            return (-1, ex.Message);
        }
    }

    [LibraryImport("advapi32.dll", EntryPoint = "OpenSCManagerW",
        StringMarshalling = StringMarshalling.Utf16, SetLastError = true)]
    private static partial IntPtr OpenSCManager(string? machine, string? database, uint access);

    [LibraryImport("advapi32.dll", EntryPoint = "OpenServiceW",
        StringMarshalling = StringMarshalling.Utf16, SetLastError = true)]
    private static partial IntPtr OpenService(IntPtr scm, string name, uint access);

    [LibraryImport("advapi32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool QueryServiceStatus(IntPtr service, out ServiceStatus status);

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
}
