using System.Runtime.InteropServices;

namespace FindMyFiles.Services;

/// <summary>Raw COM apartment boundary for the reveal STA.</summary>
internal static partial class ShellOpsNative
{
    private const uint CoInitApartmentThreaded = 0x2;

    /// <summary>Initializes COM as an STA on the current thread.</summary>
    /// <returns>The COM initialization HRESULT.</returns>
    internal static int CoInitialize() =>
        CoInitializeEx(IntPtr.Zero, CoInitApartmentThreaded);

    /// <summary>Balances a successful COM initialization.</summary>
    internal static void CoUninitialize() => CoUninitializeNative();

    [LibraryImport("ole32.dll")]
    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    private static partial int CoInitializeEx(IntPtr reserved, uint coInit);

    [LibraryImport("ole32.dll", EntryPoint = "CoUninitialize")]
    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    private static partial void CoUninitializeNative();
}
