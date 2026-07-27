using System.ComponentModel;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;

namespace FindMyFiles.Services;

/// <summary>
/// Pins every component of an MFT-sourced path and proves that its leaf is
/// still the exact NTFS file reference returned by the engine.  The lease is
/// held through shell dispatch: every component this token may delete is
/// opened with <c>DELETE</c> and without delete sharing, so a rename or
/// delete of that component fails for as long as the lease lives — that, not
/// the share mode alone, is what closes the check-to-use window (see
/// <see cref="RealIndexedShellTargetVerifier"/> for the two cases where a
/// component can only be observed, never locked).
/// </summary>
internal interface IIndexedShellTargetVerifier
{
    /// <summary>Verify and pin one indexed target until the returned lease is disposed.</summary>
    /// <param name="fullPath">MFT-sourced absolute path.</param>
    /// <param name="expectedFrn">Exact NTFS record-and-sequence identity.</param>
    /// <returns>A lease holding every path component against replacement.</returns>
    IDisposable VerifyAndPin(string fullPath, ulong expectedFrn);
}

/// <summary>Production verifier over handle-bound Win32 identity APIs.</summary>
internal sealed partial class RealIndexedShellTargetVerifier : IIndexedShellTargetVerifier
{
    private const uint Delete = 0x00010000;
    private const uint FileReadAttributes = 0x00000080;
    private const uint FileShareRead = 0x00000001;
    private const uint FileShareWrite = 0x00000002;
    private const uint OpenExisting = 3;
    private const uint FileFlagBackupSemantics = 0x02000000;
    private const uint FileFlagOpenReparsePoint = 0x00200000;
    private const uint FileAttributeReparsePoint = 0x00000400;

    /// <summary>
    /// <c>IO_REPARSE_TAG_NAME_SURROGATE_BIT</c>: set on every tag whose reparse
    /// point stands in for another named object (junction, directory/file
    /// symlink).  Windows exposes it as the <c>IsReparseTagNameSurrogate</c>
    /// macro; this constant is that macro.
    /// </summary>
    private const uint ReparseTagNameSurrogateBit = 0x2000_0000;

    private const int FileAttributeTagInfo = 9;
    private const int FileIdInfo = 18;
    private const int ErrorAccessDenied = 5;
    private const int ErrorSharingViolation = 32;

    internal static readonly RealIndexedShellTargetVerifier Instance = new();

    private RealIndexedShellTargetVerifier()
    {
    }

    public IDisposable VerifyAndPin(string fullPath, ulong expectedFrn)
    {
        if (!IsLexicallySafe(fullPath))
        {
            throw new InvalidOperationException(Loc.Get("Shell_UnsafeIndexedName"));
        }

        var handles = new List<SafeFileHandle>();
        try
        {
            foreach (var componentPath in ComponentPaths(fullPath))
            {
                var handle = OpenPinnedComponent(componentPath);
                handles.Add(handle);

                // Fail closed: a component whose reparse state cannot be read
                // is a component we cannot classify, so it is never dispatched.
                if (!GetFileAttributeTagInformation(
                        handle,
                        FileAttributeTagInfo,
                        out FileAttributeTagInformation attributes,
                        (uint)Marshal.SizeOf<FileAttributeTagInformation>()))
                {
                    throw new Win32Exception(Marshal.GetLastPInvokeError());
                }

                if (RedirectsPathResolution(attributes.FileAttributes, attributes.ReparseTag))
                {
                    throw new InvalidOperationException(Loc.Get("Shell_ReparsePointBlocked"));
                }
            }

            if (ReadFileReference(handles[^1]) != expectedFrn)
            {
                throw new IOException(Loc.Get("Shell_IdentityChanged"));
            }

            return new HandleSet(handles);
        }
        catch
        {
            DisposeAll(handles);
            throw;
        }
    }

    /// <summary>
    /// Reads the NTFS file reference (record number plus sequence number) the
    /// engine indexes for <paramref name="fullPath"/>, without pinning it.
    /// This is the identity <see cref="VerifyAndPin"/> proves, exposed so a
    /// test can pin a real file it just created.
    /// </summary>
    /// <param name="fullPath">Absolute path of an existing NTFS object.</param>
    /// <returns>The 64-bit NTFS file reference of the leaf.</returns>
    internal static ulong ReadFileReference(string fullPath)
    {
        using var handle = OpenComponent(fullPath, FileReadAttributes);
        if (handle.IsInvalid)
        {
            throw new Win32Exception(Marshal.GetLastPInvokeError());
        }

        return ReadFileReference(handle);
    }

    /// <summary>
    /// Opens one path component so the handle takes part in the Win32 share
    /// check.  <c>DELETE</c> is what makes the open register delete access
    /// with the file object; withholding <c>FILE_SHARE_DELETE</c> then makes
    /// every later rename/delete open of that component fail while the lease
    /// lives.  An attributes-only open registers nothing at all, so its share
    /// mode is decorative — the same lesson fmf-service's
    /// <c>security.rs::open_root</c> already records.  Only <c>DELETE</c> is
    /// added: data-access bits would additionally fail whenever another
    /// process legitimately holds the target open.
    /// </summary>
    /// <param name="componentPath">Absolute prefix of the path to pin.</param>
    /// <returns>The pinned handle, or an observing handle in the two degraded cases.</returns>
    private static SafeFileHandle OpenPinnedComponent(string componentPath)
    {
        var handle = OpenComponent(componentPath, FileReadAttributes | Delete);
        if (!handle.IsInvalid)
        {
            return handle;
        }

        int error = Marshal.GetLastPInvokeError();
        handle.Dispose();

        // Two components cannot be locked, and failing them would disable
        // every shell action instead of hardening it:
        //   ACCESS_DENIED     the DACL withholds DELETE from this token (true
        //                     of C:\, C:\Users, %ProgramFiles%, …).  It
        //                     withholds rename/delete from an attacker in this
        //                     token just as strictly, so the component is not
        //                     swappable to begin with.
        //   SHARING_VIOLATION someone already holds the component without
        //                     delete sharing (a process whose current
        //                     directory it is, an editor holding a document).
        //                     That holder blocks rename/delete for everyone
        //                     while it lives.
        // Both degrade to an observing open: the reparse-point and identity
        // checks still run, only the lease is weaker.  Anything else — the
        // component is gone, the volume is unreachable — stays fail-closed.
        if (error is not (ErrorAccessDenied or ErrorSharingViolation))
        {
            throw new Win32Exception(error);
        }

        var observed = OpenComponent(componentPath, FileReadAttributes);
        if (observed.IsInvalid)
        {
            int observeError = Marshal.GetLastPInvokeError();
            observed.Dispose();
            throw new Win32Exception(observeError);
        }

        return observed;
    }

    /// <summary>Opens a component with the given access, always denying delete
    /// sharing and never following a reparse point.</summary>
    /// <param name="componentPath">Absolute prefix of the path to open.</param>
    /// <param name="desiredAccess">Access mask to request.</param>
    /// <returns>The raw handle, invalid when the open failed.</returns>
    private static SafeFileHandle OpenComponent(string componentPath, uint desiredAccess) =>
        CreateFile(
            componentPath,
            desiredAccess,
            FileShareRead | FileShareWrite,
            IntPtr.Zero,
            OpenExisting,
            FileFlagBackupSemantics | FileFlagOpenReparsePoint,
            IntPtr.Zero);

    /// <summary>
    /// True when a path component's reparse point can make the path resolve
    /// somewhere other than where its spelling says — the only reparse
    /// behaviour this verifier exists to stop.
    /// <para>
    /// That behaviour is exactly a <em>name surrogate</em>: junctions and
    /// directory/file symlinks substitute another name during path parsing, so
    /// an attacker who can plant one redirects a verified-looking path at a
    /// target of their choosing.  Every other reparse tag — Cloud Files
    /// (OneDrive placeholders, on by default since Windows 10 1809 and covering
    /// Known-Folder-Moved Desktop/Documents/Pictures), HSM, deduplication,
    /// WOF/compression, AppExecLink — keeps the object exactly where it is and
    /// only changes how its data is fetched.  Rejecting those disabled "Open"
    /// and "Open file location" for entire user profiles while buying no
    /// safety, so only the surrogate bit is rejected.
    /// </para>
    /// <para>
    /// Fail-closed detail: a component that reports the reparse attribute but
    /// no tag cannot be classified, so it is treated as redirecting.
    /// </para>
    /// </summary>
    /// <param name="fileAttributes">FileAttributes from FILE_ATTRIBUTE_TAG_INFO.</param>
    /// <param name="reparseTag">ReparseTag from the same structure.</param>
    /// <returns>True when the component must not be traversed.</returns>
    internal static bool RedirectsPathResolution(uint fileAttributes, uint reparseTag)
    {
        if ((fileAttributes & FileAttributeReparsePoint) == 0)
        {
            // No reparse point at all: the tag field is meaningless here and
            // must not be consulted (Windows leaves it zero).
            return false;
        }

        return reparseTag == 0 || (reparseTag & ReparseTagNameSurrogateBit) != 0;
    }

    /// <summary>Reads the NTFS file reference behind an open handle.</summary>
    /// <param name="handle">Handle to an open NTFS object.</param>
    /// <returns>The 64-bit NTFS file reference.</returns>
    private static ulong ReadFileReference(SafeFileHandle handle)
    {
        if (!GetFileInformationByHandleEx(
                handle,
                FileIdInfo,
                out FileIdInformation identity,
                (uint)Marshal.SizeOf<FileIdInformation>()))
        {
            throw new Win32Exception(Marshal.GetLastPInvokeError());
        }

        // find-my-files indexes NTFS only.  NTFS identifiers occupy the low 64
        // bits; a non-zero high half is an unsupported identity format, never
        // something to truncate into a plausible FRN.
        if (identity.FileId.High != 0)
        {
            throw new IOException(Loc.Get("Shell_IdentityChanged"));
        }

        return identity.FileId.Low;
    }

    /// <summary>
    /// Reject spellings whose normal Win32 interpretation can normalize to a
    /// different directory entry.  NTFS can store these names, so search and
    /// copy remain available; only path-based shell actions are disabled.
    /// </summary>
    /// <param name="fullPath">Path to classify.</param>
    /// <returns>True only for an unambiguous absolute drive path.</returns>
    internal static bool IsLexicallySafe(string fullPath)
    {
        if (string.IsNullOrEmpty(fullPath)
            || fullPath.Length < 4
            || !IsAsciiLetter(fullPath[0])
            || fullPath[1] != ':'
            || fullPath[2] != '\\'
            || fullPath.StartsWith(@"\\", StringComparison.Ordinal)
            || fullPath.Contains('/', StringComparison.Ordinal))
        {
            return false;
        }

        var components = fullPath[3..].Split('\\');
        if (components.Length == 0)
        {
            return false;
        }

        foreach (var component in components)
        {
            if (component.Length == 0)
            {
                return false;
            }

            if (component is "." or ".."
                || component[^1] is ' ' or '.'
                || IsDosDeviceName(component))
            {
                return false;
            }

            for (var i = 0; i < component.Length; i++)
            {
                char ch = component[i];
                if (ch < ' ' || ch is '"' or '<' or '>' or ':' or '|' or '?' or '*')
                {
                    return false;
                }

                if (char.IsHighSurrogate(ch))
                {
                    if (i + 1 >= component.Length || !char.IsLowSurrogate(component[++i]))
                    {
                        return false;
                    }
                }
                else if (char.IsLowSurrogate(ch))
                {
                    return false;
                }
            }
        }

        try
        {
            return string.Equals(Path.GetFullPath(fullPath), fullPath, StringComparison.Ordinal);
        }
        catch (Exception ex) when (ex is ArgumentException or NotSupportedException or PathTooLongException)
        {
            return false;
        }
    }

    private static IEnumerable<string> ComponentPaths(string fullPath)
    {
        yield return fullPath[..3];
        var end = 3;
        while (end < fullPath.Length)
        {
            int separator = fullPath.IndexOf('\\', end);
            end = separator < 0 ? fullPath.Length : separator;
            yield return fullPath[..end];
            end++;
        }
    }

    /// <summary>
    /// True when Win32 would resolve the component to a DOS device instead of
    /// a directory entry.  The stem is taken up to the first dot and then
    /// stripped of trailing spaces, because Win32 resolves <c>COM1 .txt</c> to
    /// <c>COM1</c> — that trailing-space rule belongs here, to the reserved
    /// stems, and must not reject ordinary names such as <c>report .txt</c>.
    /// </summary>
    /// <param name="component">One path component.</param>
    /// <returns>True when the component names a DOS device.</returns>
    private static bool IsDosDeviceName(string component)
    {
        string stem = component.Split('.', 2)[0].TrimEnd(' ');
        if (stem.Equals("CON", StringComparison.OrdinalIgnoreCase)
            || stem.Equals("PRN", StringComparison.OrdinalIgnoreCase)
            || stem.Equals("AUX", StringComparison.OrdinalIgnoreCase)
            || stem.Equals("NUL", StringComparison.OrdinalIgnoreCase)
            || stem.Equals("CONIN$", StringComparison.OrdinalIgnoreCase)
            || stem.Equals("CONOUT$", StringComparison.OrdinalIgnoreCase))
        {
            return true;
        }

        if (stem.Length != 4)
        {
            return false;
        }

        bool numberedDevice =
            stem.StartsWith("COM", StringComparison.OrdinalIgnoreCase)
            || stem.StartsWith("LPT", StringComparison.OrdinalIgnoreCase);
        return numberedDevice
            && (stem[3] is (>= '1' and <= '9') or '¹' or '²' or '³');
    }

    private static bool IsAsciiLetter(char value) =>
        value is >= 'A' and <= 'Z' or >= 'a' and <= 'z';

    private static void DisposeAll(List<SafeFileHandle> handles)
    {
        for (var i = handles.Count - 1; i >= 0; i--)
        {
            handles[i].Dispose();
        }
    }

    private sealed class HandleSet(List<SafeFileHandle> handles) : IDisposable
    {
        private List<SafeFileHandle>? _handles = handles;

        public void Dispose()
        {
            var handlesToDispose = Interlocked.Exchange(ref _handles, null);
            if (handlesToDispose is not null)
            {
                DisposeAll(handlesToDispose);
            }
        }
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct FileId128
    {
        internal ulong Low;
        internal ulong High;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct FileIdInformation
    {
        internal ulong VolumeSerialNumber;
        internal FileId128 FileId;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct FileAttributeTagInformation
    {
        internal uint FileAttributes;
        internal uint ReparseTag;
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
    private static partial bool GetFileInformationByHandleEx(
        SafeFileHandle file,
        int informationClass,
        out FileIdInformation information,
        uint bufferSize);

    [LibraryImport(
        "kernel32.dll",
        EntryPoint = "GetFileInformationByHandleEx",
        SetLastError = true)]
    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool GetFileAttributeTagInformation(
        SafeFileHandle file,
        int informationClass,
        out FileAttributeTagInformation information,
        uint bufferSize);
}
