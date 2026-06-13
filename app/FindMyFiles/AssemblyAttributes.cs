using System.Runtime.InteropServices;

// Pin native library resolution to the OS-safe default dirs — the application
// directory (where the bundled fmf_engine.dll is copied) and System32 (the Win32
// API imports) — never the current directory or the full PATH, which are
// DLL-planting vectors. SafeDirectories is the value CA5392 requires *and* CA5393
// accepts as safe (AssemblyDirectory is rejected by CA5393). Narrower per-P/Invoke
// attributes (e.g. ShellOps' shell32 imports pinned to System32) still override it.
[assembly: DefaultDllImportSearchPaths(DllImportSearchPath.SafeDirectories)]
