using System.Buffers.Binary;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Text;
using Microsoft.Win32.SafeHandles;

namespace FindMyFiles.Tests;

/// <summary>
/// Creates a real NTFS junction so the reparse-point rules can be tested
/// against the file system that enforces them rather than against a stub.
/// <para>
/// A junction is the one name-surrogate reparse point an ordinary user can
/// create: it needs no SeCreateSymbolicLinkPrivilege and no Developer Mode
/// (both of which a symlink needs, which would make the test machine-dependent).
/// It is written with FSCTL_SET_REPARSE_POINT directly so no external process
/// (cmd's mklink) has to be spawned from a unit test.
/// </para>
/// </summary>
internal static partial class NativeJunction
{
    private const uint GenericWrite = 0x40000000;
    private const uint OpenExisting = 3;
    private const uint FileFlagBackupSemantics = 0x02000000;
    private const uint FileFlagOpenReparsePoint = 0x00200000;
    private const uint FsctlSetReparsePoint = 0x000900A4;
    private const uint IoReparseTagMountPoint = 0xA0000003;

    /// <summary>Points <paramref name="junctionPath"/> at an existing directory.</summary>
    internal static void Create(string junctionPath, string targetDirectory)
    {
        // A junction lives in a directory entry that already exists: create the
        // empty directory first, then stamp the reparse point onto it.
        Directory.CreateDirectory(junctionPath);

        var payload = BuildMountPointBuffer(targetDirectory);
        using var handle = CreateFile(
            junctionPath,
            GenericWrite,
            0,
            IntPtr.Zero,
            OpenExisting,
            FileFlagBackupSemantics | FileFlagOpenReparsePoint,
            IntPtr.Zero);
        if (handle.IsInvalid)
        {
            throw new Win32Exception(Marshal.GetLastPInvokeError());
        }

        var native = Marshal.AllocHGlobal(payload.Length);
        try
        {
            Marshal.Copy(payload, 0, native, payload.Length);
            if (!DeviceIoControl(
                    handle,
                    FsctlSetReparsePoint,
                    native,
                    (uint)payload.Length,
                    IntPtr.Zero,
                    0,
                    out _,
                    IntPtr.Zero))
            {
                throw new Win32Exception(Marshal.GetLastPInvokeError());
            }
        }
        finally
        {
            Marshal.FreeHGlobal(native);
        }
    }

    /// <summary>
    /// Lays out REPARSE_DATA_BUFFER's MountPointReparseBuffer variant: an
    /// 8-byte header (tag, data length, reserved), four 16-bit name
    /// offsets/lengths, then the NUL-terminated substitute name followed by the
    /// NUL-terminated print name.
    /// </summary>
    private static byte[] BuildMountPointBuffer(string targetDirectory)
    {
        // The substitute name is what the object manager resolves, so it uses
        // the \??\ (DosDevices) prefix; the print name is what tools display.
        var substitute = Encoding.Unicode.GetBytes(@"\??\" + targetDirectory);
        var print = Encoding.Unicode.GetBytes(targetDirectory);
        int names = substitute.Length + 2 + print.Length + 2;
        int dataLength = 8 + names;
        var buffer = new byte[8 + dataLength];

        BinaryPrimitives.WriteUInt32LittleEndian(buffer.AsSpan(0), IoReparseTagMountPoint);
        BinaryPrimitives.WriteUInt16LittleEndian(buffer.AsSpan(4), (ushort)dataLength);
        BinaryPrimitives.WriteUInt16LittleEndian(buffer.AsSpan(6), 0); // Reserved
        BinaryPrimitives.WriteUInt16LittleEndian(buffer.AsSpan(8), 0); // SubstituteNameOffset
        BinaryPrimitives.WriteUInt16LittleEndian(buffer.AsSpan(10), (ushort)substitute.Length);
        BinaryPrimitives.WriteUInt16LittleEndian(
            buffer.AsSpan(12),
            (ushort)(substitute.Length + 2)); // PrintNameOffset
        BinaryPrimitives.WriteUInt16LittleEndian(buffer.AsSpan(14), (ushort)print.Length);
        substitute.AsSpan().CopyTo(buffer.AsSpan(16));
        print.AsSpan().CopyTo(buffer.AsSpan(16 + substitute.Length + 2));
        return buffer;
    }

    [LibraryImport(
        "kernel32.dll",
        EntryPoint = "CreateFileW",
        SetLastError = true,
        StringMarshalling = StringMarshalling.Utf16)]
    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    private static partial SafeFileHandle CreateFile(
        string fileName,
        uint desiredAccess,
        uint shareMode,
        IntPtr securityAttributes,
        uint creationDisposition,
        uint flagsAndAttributes,
        IntPtr templateFile);

    [LibraryImport("kernel32.dll", SetLastError = true)]
    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool DeviceIoControl(
        SafeFileHandle device,
        uint controlCode,
        IntPtr inBuffer,
        uint inBufferSize,
        IntPtr outBuffer,
        uint outBufferSize,
        out uint bytesReturned,
        IntPtr overlapped);
}
