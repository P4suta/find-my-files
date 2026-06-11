using System.Runtime.InteropServices;
using System.Security.Principal;
using Microsoft.Win32.SafeHandles;

namespace FindMyFiles.Engine;

/// <summary>
/// Client-side check that the process at the far end of the default pipe
/// runs as SYSTEM (S-1-5-18) — SECURITY.md 脅威4 (pipe-name squatting / fake
/// server). Deliberately separate from <see cref="NativeEngine"/>: these are
/// OS bindings, unrelated to fmf_engine.dll. Every failure answers false
/// (fail closed).
/// </summary>
internal static partial class PipeServerIdentity
{
    private const uint ProcessQueryLimitedInformation = 0x1000;
    private const uint TokenQuery = 0x0008;
    private const int TokenUserClass = 1; // TOKEN_INFORMATION_CLASS.TokenUser

    /// <summary>True only when the pipe's server process token is SYSTEM.</summary>
    internal static bool IsServerSystem(SafePipeHandle pipe) =>
        GetNamedPipeServerProcessId(pipe, out var pid) && IsProcessSystem(pid);

    /// <summary>The token check behind <see cref="IsServerSystem"/>, split
    /// out so unit tests can exercise it without a live pipe.</summary>
    internal static bool IsProcessSystem(uint pid)
    {
        var process = OpenProcess(ProcessQueryLimitedInformation, false, pid);
        if (process == IntPtr.Zero)
        {
            return false;
        }
        try
        {
            if (!OpenProcessToken(process, TokenQuery, out var token))
            {
                return false;
            }
            try
            {
                return TokenUserIsSystem(token);
            }
            finally
            {
                CloseHandle(token);
            }
        }
        finally
        {
            CloseHandle(process);
        }
    }

    private static bool TokenUserIsSystem(IntPtr token)
    {
        GetTokenInformation(token, TokenUserClass, IntPtr.Zero, 0, out var size);
        if (size == 0)
        {
            return false;
        }
        var buffer = Marshal.AllocHGlobal((int)size);
        try
        {
            if (!GetTokenInformation(token, TokenUserClass, buffer, size, out _))
            {
                return false;
            }
            // TOKEN_USER = { SID_AND_ATTRIBUTES User } — the first field is
            // the PSID pointer.
            var sid = Marshal.ReadIntPtr(buffer);
            return new SecurityIdentifier(sid).IsWellKnown(WellKnownSidType.LocalSystemSid);
        }
        catch (ArgumentException)
        {
            return false; // unparsable SID — never trust it
        }
        finally
        {
            Marshal.FreeHGlobal(buffer);
        }
    }

    [LibraryImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool GetNamedPipeServerProcessId(
        SafePipeHandle pipe, out uint serverProcessId);

    [LibraryImport("kernel32.dll", SetLastError = true)]
    private static partial IntPtr OpenProcess(
        uint desiredAccess, [MarshalAs(UnmanagedType.Bool)] bool inheritHandle, uint processId);

    [LibraryImport("advapi32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool OpenProcessToken(
        IntPtr process, uint desiredAccess, out IntPtr token);

    [LibraryImport("advapi32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool GetTokenInformation(
        IntPtr token, int infoClass, IntPtr info, uint infoLen, out uint returnLen);

    [LibraryImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool CloseHandle(IntPtr handle);
}
