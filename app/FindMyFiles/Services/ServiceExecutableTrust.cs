using System.ComponentModel;
using System.Reflection;
using System.Runtime.InteropServices;
using System.Security;
using System.Security.Cryptography;
using Microsoft.Win32.SafeHandles;

namespace FindMyFiles.Services;

/// <summary>
/// Fail-closed trust boundary for the companion executable that crosses UAC.
/// The exact PE image is pinned at publish time using Windows' Authenticode
/// digest stream, so release signing can add its certificate without changing
/// the identity. Handles deny replacement/rename from validation through
/// process creation.
/// </summary>
internal static unsafe partial class ServiceExecutableTrust
{
    private const uint GenericRead = 0x80000000;
    private const uint FileReadAttributes = 0x00000080;
    private const uint FileShareRead = 0x00000001;
    private const uint OpenExisting = 3;
    private const uint FileFlagBackupSemantics = 0x02000000;
    private const uint FileFlagOpenReparsePoint = 0x00200000;
    private const uint FileFlagSequentialScan = 0x08000000;
    private const uint FileAttributeDirectory = 0x00000010;
    private const uint FileAttributeReparsePoint = 0x00000400;
    private const int FileAttributeTagInfoClass = 9;
    private const uint DigestLevelAll = 0x01 | 0x02 | 0x04;
    private const uint WinTrustUiNone = 2;
    private const uint WinTrustRevokeNone = 0;
    private const uint WinTrustChoiceFile = 1;
    private const uint WinTrustStateActionIgnore = 0;
    private const uint WinTrustCacheOnlyUrlRetrieval = 0x00001000;
    private const uint WinTrustRevocationCheckNone = 0x00000010;

    private static readonly Lock ImageHlpLock = new();
    private static readonly DigestCallback DigestCallbackRoot = AppendDigest;
    private static readonly IntPtr DigestCallbackPointer =
        Marshal.GetFunctionPointerForDelegate(DigestCallbackRoot);

    internal static string? ExpectedImageSha256 =>
        typeof(ServiceExecutableTrust).Assembly
            .GetCustomAttributes<AssemblyMetadataAttribute>()
            .SingleOrDefault(
                attribute => string.Equals(
                    attribute.Key,
                    "FmfServiceImageSha256",
                    StringComparison.Ordinal))
            ?.Value;

    internal static bool IsPinnedDigest(string? value) =>
        value is { Length: 64 }
        && value.All(static character =>
            character is >= '0' and <= '9'
            or >= 'a' and <= 'f');

    internal static ServiceExecutableLease Acquire(string path)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(path);
        var expectedHex = ExpectedImageSha256;
        if (!IsPinnedDigest(expectedHex))
        {
            throw new SecurityException(
                "This build does not pin an fmf-service.exe image. "
                + "Use the canonical publish pipeline.");
        }

        var fullPath = Path.GetFullPath(path);
        if (!string.Equals(
                Path.GetFileName(fullPath),
                "fmf-service.exe",
                StringComparison.OrdinalIgnoreCase))
        {
            throw new SecurityException("The elevated companion has an unexpected filename.");
        }

        var lease = new ServiceExecutableLease(fullPath);
        try
        {
            lease.LockParentDirectories();
            lease.OpenImage();
            var actual = ComputeImageSha256(lease.ImageHandle);
            var expected = Convert.FromHexString(expectedHex!);
            if (!CryptographicOperations.FixedTimeEquals(actual, expected))
            {
                throw new SecurityException(
                    "fmf-service.exe does not match the service image pinned by this app.");
            }

            VerifyAuthenticode(lease.ImageHandle, fullPath);
            return lease;
        }
        catch
        {
            lease.Dispose();
            throw;
        }
    }

    private static byte[] ComputeImageSha256(SafeFileHandle image)
    {
        using var state = new DigestState();
        var stateHandle = GCHandle.Alloc(state);
        try
        {
            bool succeeded;
            lock (ImageHlpLock)
            {
                succeeded = ImageGetDigestStream(
                    image.DangerousGetHandle(),
                    DigestLevelAll,
                    DigestCallbackPointer,
                    GCHandle.ToIntPtr(stateHandle));
            }

            if (!succeeded)
            {
                throw new Win32Exception(
                    Marshal.GetLastWin32Error(),
                    "ImageGetDigestStream rejected fmf-service.exe.");
            }

            if (state.Error is not null)
            {
                throw new SecurityException(
                    "Could not hash the complete fmf-service.exe image.",
                    state.Error);
            }

            return state.Hash.GetHashAndReset();
        }
        finally
        {
            stateHandle.Free();
        }
    }

    private static unsafe int AppendDigest(IntPtr digestHandle, byte* data, uint length)
    {
        try
        {
            var state = (DigestState?)GCHandle.FromIntPtr(digestHandle).Target;
            if (state is null || (data is null && length != 0))
            {
                return 0;
            }

            state.Hash.AppendData(
                length == 0
                    ? []
                    : new ReadOnlySpan<byte>(data, checked((int)length)));
            return 1;
        }
        catch (Exception ex)
        {
            if (digestHandle != IntPtr.Zero
                && GCHandle.FromIntPtr(digestHandle).Target is DigestState state)
            {
                state.Error = ex;
            }

            return 0;
        }
    }

    private static void VerifyAuthenticode(SafeFileHandle image, string path)
    {
        var pathPointer = Marshal.StringToCoTaskMemUni(path);
        var fileInfoPointer = IntPtr.Zero;
        var trustDataPointer = IntPtr.Zero;
        try
        {
            var fileInfo = new WinTrustFileInfo
            {
                StructSize = (uint)Marshal.SizeOf<WinTrustFileInfo>(),
                FilePath = pathPointer,
                FileHandle = image.DangerousGetHandle(),
                KnownSubject = IntPtr.Zero,
            };
            fileInfoPointer = Marshal.AllocHGlobal(Marshal.SizeOf<WinTrustFileInfo>());
            Marshal.StructureToPtr(fileInfo, fileInfoPointer, false);

            var trustData = new WinTrustData
            {
                StructSize = (uint)Marshal.SizeOf<WinTrustData>(),
                UiChoice = WinTrustUiNone,
                RevocationChecks = WinTrustRevokeNone,
                UnionChoice = WinTrustChoiceFile,
                FileInfo = fileInfoPointer,
                StateAction = WinTrustStateActionIgnore,
                ProviderFlags =
                    WinTrustCacheOnlyUrlRetrieval | WinTrustRevocationCheckNone,
            };
            trustDataPointer = Marshal.AllocHGlobal(Marshal.SizeOf<WinTrustData>());
            Marshal.StructureToPtr(trustData, trustDataPointer, false);

            var action = new Guid("00AAC56B-CD44-11D0-8CC2-00C04FC295EE");
            var status = WinVerifyTrust(new IntPtr(-1), in action, trustDataPointer);
            if (status != 0)
            {
                throw new SecurityException(
                    $"fmf-service.exe does not have a trusted Authenticode signature "
                    + $"(0x{status:X8}).");
            }
        }
        finally
        {
            if (trustDataPointer != IntPtr.Zero)
            {
                Marshal.FreeHGlobal(trustDataPointer);
            }

            if (fileInfoPointer != IntPtr.Zero)
            {
                Marshal.FreeHGlobal(fileInfoPointer);
            }

            Marshal.FreeCoTaskMem(pathPointer);
        }
    }

    private static SafeFileHandle OpenAndValidate(
        string path,
        bool directory)
    {
        var flags = FileFlagOpenReparsePoint
            | (directory ? FileFlagBackupSemantics : FileFlagSequentialScan);
        var handle = CreateFile(
            path,
            directory ? FileReadAttributes : GenericRead,
            FileShareRead,
            IntPtr.Zero,
            OpenExisting,
            flags,
            IntPtr.Zero);
        if (handle.IsInvalid)
        {
            var error = Marshal.GetLastWin32Error();
            handle.Dispose();
            throw new Win32Exception(error, $"Could not lock {path} for verification.");
        }

        if (!GetFileInformationByHandleEx(
                handle,
                FileAttributeTagInfoClass,
                out var info,
                (uint)Marshal.SizeOf<FileAttributeTagInfo>()))
        {
            var error = Marshal.GetLastWin32Error();
            handle.Dispose();
            throw new Win32Exception(error, $"Could not inspect {path}.");
        }

        var isDirectory = (info.FileAttributes & FileAttributeDirectory) != 0;
        if ((info.FileAttributes & FileAttributeReparsePoint) != 0
            || isDirectory != directory)
        {
            handle.Dispose();
            throw new SecurityException(
                $"{path} is a reparse point or has an unexpected file type.");
        }

        return handle;
    }

    [UnmanagedFunctionPointer(CallingConvention.Winapi)]
    private unsafe delegate int DigestCallback(
        IntPtr digestHandle,
        byte* data,
        uint length);

    private sealed class DigestState : IDisposable
    {
        public IncrementalHash Hash { get; } =
            IncrementalHash.CreateHash(HashAlgorithmName.SHA256);

        public Exception? Error { get; set; }

        public void Dispose() => Hash.Dispose();
    }

    internal sealed class ServiceExecutableLease : IDisposable
    {
        private readonly List<SafeFileHandle> _directoryHandles = [];

        internal ServiceExecutableLease(string path)
        {
            Path = path;
        }

        internal string Path { get; }

        internal SafeFileHandle ImageHandle { get; private set; } =
            new(IntPtr.Zero, ownsHandle: false);

        internal void LockParentDirectories()
        {
            var directory = Directory.GetParent(Path)
                ?? throw new SecurityException("The service image has no parent directory.");
            while (directory.Parent is not null)
            {
                _directoryHandles.Add(OpenAndValidate(directory.FullName, directory: true));
                directory = directory.Parent;
            }
        }

        internal void OpenImage()
        {
            ImageHandle.Dispose();
            ImageHandle = OpenAndValidate(Path, directory: false);
        }

        public void Dispose()
        {
            ImageHandle.Dispose();
            foreach (var handle in _directoryHandles)
            {
                handle.Dispose();
            }

            _directoryHandles.Clear();
        }
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct FileAttributeTagInfo
    {
        public uint FileAttributes;
        public uint ReparseTag;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct WinTrustFileInfo
    {
        public uint StructSize;
        public IntPtr FilePath;
        public IntPtr FileHandle;
        public IntPtr KnownSubject;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct WinTrustData
    {
        public uint StructSize;
        public IntPtr PolicyCallbackData;
        public IntPtr SipClientData;
        public uint UiChoice;
        public uint RevocationChecks;
        public uint UnionChoice;
        public IntPtr FileInfo;
        public uint StateAction;
        public IntPtr StateData;
        public IntPtr UrlReference;
        public uint ProviderFlags;
        public uint UiContext;
        public IntPtr SignatureSettings;
    }

    [LibraryImport(
        "kernel32.dll",
        EntryPoint = "CreateFileW",
        StringMarshalling = StringMarshalling.Utf16,
        SetLastError = true)]
    private static partial SafeFileHandle CreateFile(
        string fileName,
        uint desiredAccess,
        uint shareMode,
        IntPtr securityAttributes,
        uint creationDisposition,
        uint flagsAndAttributes,
        IntPtr templateFile);

    [LibraryImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool GetFileInformationByHandleEx(
        SafeFileHandle file,
        int informationClass,
        out FileAttributeTagInfo information,
        uint bufferSize);

    [LibraryImport("imagehlp.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool ImageGetDigestStream(
        IntPtr file,
        uint digestLevel,
        IntPtr digestFunction,
        IntPtr digestHandle);

    [LibraryImport("wintrust.dll")]
    private static partial int WinVerifyTrust(
        IntPtr window,
        in Guid action,
        IntPtr trustData);
}
