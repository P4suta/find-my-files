using System.Runtime.InteropServices;

namespace FindMyFiles.Services;

/// <summary>The tray icon's right-click menu (ADR-0030): a Win32
/// <c>TrackPopupMenuEx</c> popup, since a WinUI <c>MenuFlyout</c> cannot be shown
/// outside an HWND / XamlRoot context. Synchronous (<c>TPM_RETURNCMD</c>): it
/// returns the chosen command and the caller acts on it. Labels are localized via
/// <see cref="Loc"/>. Self-written P/Invoke, pinned to System32, like the other
/// interop seams (<see cref="ServiceSetup"/>, <c>ShellOps</c>).</summary>
internal static partial class TrayMenu
{
    private const uint MfString = 0x0;
    private const uint TpmRightButton = 0x2;
    private const uint TpmReturnCmd = 0x100;
    private const uint TpmNoNotify = 0x80;
    private const uint CmdOpen = 1;
    private const uint CmdExit = 2;

    /// <summary>Shows the context menu at the cursor and returns the chosen
    /// command.</summary>
    /// <param name="hwnd">Owner window — foregrounded first so the menu does not
    /// dismiss immediately (the classic shell tray-menu workaround).</param>
    /// <returns>The selected command, or <see cref="TrayCommand.None"/>.</returns>
    internal static TrayCommand Show(IntPtr hwnd)
    {
        var menu = CreatePopupMenu();
        if (menu == IntPtr.Zero)
        {
            return TrayCommand.None;
        }

        try
        {
            _ = AppendMenu(menu, MfString, CmdOpen, Loc.Get("Tray_Open"));
            _ = AppendMenu(menu, MfString, CmdExit, Loc.Get("Tray_Exit"));

            // Foreground the owner first or the menu closes the instant it opens.
            _ = SetForegroundWindow(hwnd);
            _ = GetCursorPos(out var pt);

            var cmd = TrackPopupMenuEx(
                menu, TpmRightButton | TpmReturnCmd | TpmNoNotify, pt.X, pt.Y, hwnd, IntPtr.Zero);
            return cmd switch
            {
                (int)CmdOpen => TrayCommand.Open,
                (int)CmdExit => TrayCommand.Exit,
                _ => TrayCommand.None,
            };
        }
        finally
        {
            _ = DestroyMenu(menu);
        }
    }

    [LibraryImport("user32.dll", SetLastError = true)]
    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    private static partial IntPtr CreatePopupMenu();

    [LibraryImport("user32.dll", EntryPoint = "AppendMenuW",
        StringMarshalling = StringMarshalling.Utf16, SetLastError = true)]
    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool AppendMenu(IntPtr hMenu, uint uFlags, nuint uIDNewItem, string lpNewItem);

    [LibraryImport("user32.dll", SetLastError = true)]
    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    private static partial int TrackPopupMenuEx(IntPtr hMenu, uint uFlags, int x, int y, IntPtr hwnd, IntPtr lptpm);

    [LibraryImport("user32.dll", SetLastError = true)]
    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool DestroyMenu(IntPtr hMenu);

    [LibraryImport("user32.dll", SetLastError = true)]
    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool SetForegroundWindow(IntPtr hWnd);

    [LibraryImport("user32.dll", SetLastError = true)]
    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool GetCursorPos(out NativePoint lpPoint);

    [StructLayout(LayoutKind.Sequential)]
    private struct NativePoint
    {
        public int X;
        public int Y;
    }
}

/// <summary>A command chosen from the tray context menu.</summary>
internal enum TrayCommand
{
    /// <summary>No selection (the menu was dismissed).</summary>
    None,

    /// <summary>Show / restore the main window.</summary>
    Open,

    /// <summary>Really exit the application.</summary>
    Exit,
}
