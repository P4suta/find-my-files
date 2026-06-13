using System.Runtime.InteropServices;

// Match the app: pin native library resolution to the OS-safe default dirs so the
// WinAppSDK bootstrap P/Invoke (compiled into this test assembly) never searches the
// current directory or full PATH — DLL-planting vectors (CA5392 / CA5393).
[assembly: DefaultDllImportSearchPaths(DllImportSearchPath.SafeDirectories)]
