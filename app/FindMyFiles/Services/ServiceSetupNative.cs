using System.Diagnostics;
using System.Runtime.InteropServices;

namespace FindMyFiles.Services;

/// <summary>Raw SCM and process adapters. Policy and outcome classification
/// remain in <see cref="ServiceSetup"/>.</summary>
internal static partial class ServiceSetupNative
{
    internal static IServiceManagerHandle? OpenManager(uint access)
    {
        var handle = OpenSCManager(null, null, access);
        return handle == IntPtr.Zero ? null : new NativeServiceManager(handle);
    }

    internal static IElevatedProcess? StartProcess(ProcessStartInfo startInfo)
    {
        var process = Process.Start(startInfo);
        return process is null ? null : new NativeElevatedProcess(process);
    }

    private sealed class NativeServiceManager(IntPtr handle) : IServiceManagerHandle
    {
        public int LastError { get; private set; }

        public IServiceHandle? OpenService(string name, uint access)
        {
            var service = OpenServiceNative(handle, name, access);
            if (service == IntPtr.Zero)
            {
                LastError = Marshal.GetLastWin32Error();
                return null;
            }

            return new NativeServiceHandle(service);
        }

        public void Dispose() => CloseServiceHandle(handle);
    }

    private sealed class NativeServiceHandle(IntPtr handle) : IServiceHandle
    {
        public bool TryQueryState(out uint state)
        {
            var success = QueryServiceStatus(handle, out var status);
            state = status.CurrentState;
            return success;
        }

        public uint QueryDescriptionBytesNeeded()
        {
            _ = QueryServiceConfig2(handle, 1, IntPtr.Zero, 0, out var bytesNeeded);
            return bytesNeeded;
        }

        public bool TryReadDescription(uint bytesNeeded, out string? description)
        {
            var buffer = Marshal.AllocHGlobal(checked((int)bytesNeeded));
            try
            {
                if (!QueryServiceConfig2(handle, 1, buffer, bytesNeeded, out _))
                {
                    description = null;
                    return false;
                }

                var native = Marshal.PtrToStructure<ServiceDescription>(buffer);
                description = Marshal.PtrToStringUni(native.Description);
                return true;
            }
            finally
            {
                Marshal.FreeHGlobal(buffer);
            }
        }

        public bool TryQueryProcess(out uint state, out uint processId)
        {
            var size = (uint)Marshal.SizeOf<ServiceStatusProcess>();
            var buffer = Marshal.AllocHGlobal(checked((int)size));
            try
            {
                if (!QueryServiceStatusEx(handle, 0, buffer, size, out _))
                {
                    state = 0;
                    processId = 0;
                    return false;
                }

                var status = Marshal.PtrToStructure<ServiceStatusProcess>(buffer);
                state = status.CurrentState;
                processId = status.ProcessId;
                return true;
            }
            finally
            {
                Marshal.FreeHGlobal(buffer);
            }
        }

        public int Start() =>
            StartServiceNative(handle, 0, IntPtr.Zero) ? 0 : Marshal.GetLastWin32Error();

        public int Stop() =>
            ControlServiceNative(handle, 1, out _) ? 0 : Marshal.GetLastWin32Error();

        public void Dispose() => CloseServiceHandle(handle);
    }

    private sealed class NativeElevatedProcess(Process process) : IElevatedProcess
    {
        public int ExitCode => process.ExitCode;

        public int Id => process.Id;

        public bool WaitForExit(int milliseconds) => process.WaitForExit(milliseconds);

        public void Kill(bool entireProcessTree) => process.Kill(entireProcessTree);

        public void Dispose() => process.Dispose();
    }

    [LibraryImport("advapi32.dll", EntryPoint = "OpenSCManagerW",
        StringMarshalling = StringMarshalling.Utf16, SetLastError = true)]
    private static partial IntPtr OpenSCManager(string? machine, string? database, uint access);

    [LibraryImport("advapi32.dll", EntryPoint = "OpenServiceW",
        StringMarshalling = StringMarshalling.Utf16, SetLastError = true)]
    private static partial IntPtr OpenServiceNative(IntPtr scm, string name, uint access);

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
