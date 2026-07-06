using System.Runtime.InteropServices;
using Windows.ApplicationModel;

namespace FindMyFiles.Services;

/// <summary>
/// Whether this process runs with MSIX package identity, and where the package
/// is installed. ADR-0028 ships the SAME UI two ways — the portable zip
/// (unpackaged) and the signed <c>.msix</c> (packaged) — and a few paths must
/// diverge under identity: <see cref="AppPaths"/> forces the per-user profile
/// (the install dir is read-only), and <see cref="ServiceSetup"/> sources the
/// bundled <c>fmf-service.exe</c> from the install location (R3).
/// <para>Like <see cref="AppPaths"/>, this must NOT call <see cref="FileLog"/>:
/// AppPaths consumes it and FileLog's directory comes from AppPaths (a cycle).
/// The identity is fixed for the process lifetime, so it is cached once.</para>
/// </summary>
public static partial class PackageIdentity
{
    /// <summary>Win32 <c>APPMODEL_ERROR_NO_PACKAGE</c> — returned by
    /// <c>GetCurrentPackageFullName</c> when the process has no package identity.</summary>
    private const int AppModelErrorNoPackage = 15700;

    private static readonly Lazy<bool> PackagedLazy = new(DetectPackaged);

    /// <summary>True when the process runs under an MSIX package identity (the
    /// <c>.msix</c> install); false for the portable zip build and for tests.</summary>
    public static bool IsPackaged => PackagedLazy.Value;

    /// <summary>The package install directory (read-only, under
    /// <c>%ProgramFiles%\WindowsApps</c>) when packaged; <see langword="null"/>
    /// otherwise. This is where the bundled <c>fmf-service.exe</c> content payload
    /// lives — the source the elevated helper copies into <c>%ProgramData%</c>
    /// (ADR-0028 R3).</summary>
    public static string? InstalledLocationPath =>
        IsPackaged ? Package.Current.InstalledLocation.Path : null;

    /// <summary>Probe package identity via the App Model API. A length-only call
    /// with a null buffer returns <c>APPMODEL_ERROR_NO_PACKAGE</c> when unpackaged
    /// and something else (<c>ERROR_INSUFFICIENT_BUFFER</c>) when packaged — we
    /// only need the presence bit, so the buffer is never allocated. Using the API
    /// (not <see cref="Package"/>.Current, which THROWS when unpackaged) keeps the
    /// unpackaged path exception-free.</summary>
    private static bool DetectPackaged()
    {
        uint length = 0;

        // Null buffer (IntPtr.Zero) keeps the P/Invoke fully blittable — the
        // project disables runtime marshalling, so a char[] out-param would not
        // source-generate (SYSLIB1051). We only read the return code anyway.
        return GetCurrentPackageFullName(ref length, IntPtr.Zero) != AppModelErrorNoPackage;
    }

    [LibraryImport("kernel32.dll", EntryPoint = "GetCurrentPackageFullName")]
    private static partial int GetCurrentPackageFullName(ref uint length, IntPtr packageFullName);
}
