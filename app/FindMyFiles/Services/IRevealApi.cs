using System.Runtime.InteropServices;

namespace FindMyFiles.Services;

/// <summary>Seam over the three shell calls that reveal-and-select a path
/// (<c>SHParseDisplayName</c> → <c>SHOpenFolderAndSelectItems</c> → free the
/// PIDL). Extracted so <see cref="ShellOps.DoReveal"/>'s HRESULT handling is
/// unit-testable with a fake: the real call needs a live shell and an STA, so
/// only its orchestration can be covered without launching Explorer.</summary>
internal interface IRevealApi
{
    /// <summary>Parse <paramref name="path"/> into an absolute PIDL. On success
    /// (HRESULT <c>S_OK</c>) <paramref name="pidl"/> is owned by the caller and
    /// must be released with <see cref="FreePidl"/>.</summary>
    /// <param name="path">Absolute path to parse.</param>
    /// <param name="pidl">Receives the parsed PIDL (zero on failure).</param>
    /// <returns>The HRESULT.</returns>
    int ParseDisplayName(string path, out IntPtr pidl);

    /// <summary>Open the PIDL's parent folder and select the item. Only
    /// <c>S_OK</c> means a window was actually shown.</summary>
    /// <param name="pidl">PIDL from <see cref="ParseDisplayName"/>.</param>
    /// <returns>The HRESULT.</returns>
    int OpenFolderAndSelectItems(IntPtr pidl);

    /// <summary>Release a PIDL obtained from <see cref="ParseDisplayName"/>.</summary>
    /// <param name="pidl">The PIDL to free.</param>
    void FreePidl(IntPtr pidl);
}

/// <summary>Production <see cref="IRevealApi"/> over <c>shell32.dll</c>, pinned
/// to System32 (the same binary-planting defence ShellOps applies to
/// explorer.exe). Paths travel as data to <c>SHParseDisplayName</c>, never a
/// command line, so a quote in an MFT-sourced name cannot inject explorer
/// switches.</summary>
internal sealed partial class RealRevealApi : IRevealApi
{
    /// <summary>Shared instance — the shell entry points are stateless.</summary>
    internal static readonly RealRevealApi Instance = new();

    /// <inheritdoc/>
    public int ParseDisplayName(string path, out IntPtr pidl) =>
        SHParseDisplayName(path, IntPtr.Zero, out pidl, 0, out _);

    /// <inheritdoc/>
    public int OpenFolderAndSelectItems(IntPtr pidl) =>
        SHOpenFolderAndSelectItems(pidl, 0, null, 0);

    /// <inheritdoc/>
    public void FreePidl(IntPtr pidl) => Marshal.FreeCoTaskMem(pidl);

    [LibraryImport("shell32.dll", StringMarshalling = StringMarshalling.Utf16)]
    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    private static partial int SHParseDisplayName(
        string name, IntPtr bindingContext, out IntPtr pidl, uint sfgaoIn, out uint psfgaoOut);

    [LibraryImport("shell32.dll")]
    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    private static partial int SHOpenFolderAndSelectItems(
        IntPtr pidlFolder, uint cidl, IntPtr[]? apidl, uint dwFlags);
}
