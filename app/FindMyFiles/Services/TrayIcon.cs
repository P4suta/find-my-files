using System.Runtime.InteropServices;

namespace FindMyFiles.Services;

/// <summary>
/// The system-tray (notification area) icon (ADR-0030). Adds a Shell_NotifyIcon
/// entry bound to the main window's HWND, routes its callback message (left-click
/// → activate, right-click → <see cref="TrayMenu"/>), re-adds itself when Explorer
/// restarts (<c>TaskbarCreated</c>), and removes the icon on <see cref="Dispose"/>.
/// The HICON and the message subclass are held for the object's lifetime
/// (CLAUDE.md: native callbacks / handles are field-held). Self-written P/Invoke
/// rather than a NuGet, matching <see cref="ServiceSetup"/> and <c>ShellOps</c>.
/// </summary>
internal sealed partial class TrayIcon : IDisposable
{
    private const uint TrayUid = 1;
    private const int SubclassId = 1;
    private const uint CallbackMessage = 0x8000 + 1; // WM_APP + 1

    private const uint NimAdd = 0x0;
    private const uint NimModify = 0x1;
    private const uint NimDelete = 0x2;
    private const uint NifMessage = 0x1;
    private const uint NifIcon = 0x2;
    private const uint NifTip = 0x4;

    private const uint WmLButtonUp = 0x0202;
    private const uint WmLButtonDblClk = 0x0203;
    private const uint WmRButtonUp = 0x0205;

    private const uint ImageIcon = 1;
    private const uint LrLoadFromFile = 0x10;
    private const uint LrDefaultSize = 0x40;

    private readonly IntPtr _hwnd;
    private readonly string _tooltip;
    private readonly Action _onActivate;
    private readonly Action _onExit;
    private readonly uint _taskbarCreated;
    private readonly WindowSubclass _subclass;
    private IntPtr _hIcon;
    private bool _added;
    private bool _disposed;

    /// <summary>Creates and shows the tray icon for <paramref name="hwnd"/>.</summary>
    /// <param name="hwnd">The main window handle the icon is bound to.</param>
    /// <param name="tooltip">Hover tooltip text (truncated to 127 chars).</param>
    /// <param name="onActivate">Invoked on left-click / "Open" — show the window.</param>
    /// <param name="onExit">Invoked on the menu's "Exit" — really quit.</param>
    internal TrayIcon(IntPtr hwnd, string tooltip, Action onActivate, Action onExit)
    {
        _hwnd = hwnd;
        _tooltip = tooltip;
        _onActivate = onActivate;
        _onExit = onExit;
        _taskbarCreated = RegisterWindowMessage("TaskbarCreated");
        _hIcon = LoadTrayIcon();
        _subclass = new WindowSubclass(hwnd, SubclassId, OnMessage);
        Add();
    }

    /// <summary>Removes the icon and releases the subclass and HICON. Idempotent.</summary>
    public unsafe void Dispose()
    {
        if (_disposed)
        {
            return;
        }

        _disposed = true;

        if (_added)
        {
            var data = new NotifyIconData
            {
                CbSize = (uint)sizeof(NotifyIconData),
                HWnd = _hwnd,
                Uid = TrayUid,
            };
            _ = Shell_NotifyIcon(NimDelete, ref data);
            _added = false;
        }

        _subclass.Dispose();

        if (_hIcon != IntPtr.Zero)
        {
            _ = DestroyIcon(_hIcon);
            _hIcon = IntPtr.Zero;
        }
    }

    private bool OnMessage(uint msg, IntPtr wParam, IntPtr lParam)
    {
        if (msg == _taskbarCreated)
        {
            // Explorer restarted — the icon was lost; re-add it. Not exclusively
            // ours, so don't consume the broadcast.
            _added = false;
            Add();
            return false;
        }

        if (msg != CallbackMessage)
        {
            return false;
        }

        // Old-style callback: the mouse message is the low word of lParam.
        var mouse = (uint)(lParam.ToInt64() & 0xFFFF);
        switch (mouse)
        {
            case WmLButtonUp:
            case WmLButtonDblClk:
                _onActivate();
                break;
            case WmRButtonUp:
                switch (TrayMenu.Show(_hwnd))
                {
                    case TrayCommand.Open:
                        _onActivate();
                        break;
                    case TrayCommand.Exit:
                        _onExit();
                        break;
                    default:
                        break;
                }

                break;
            default:
                break;
        }

        return true;
    }

    private unsafe void Add()
    {
        var data = new NotifyIconData
        {
            CbSize = (uint)sizeof(NotifyIconData),
            HWnd = _hwnd,
            Uid = TrayUid,
            UFlags = NifMessage | NifIcon | NifTip,
            UCallbackMessage = CallbackMessage,
            HIcon = _hIcon,
        };

        var n = Math.Min(_tooltip.Length, 127);
        for (var i = 0; i < n; i++)
        {
            data.SzTip[i] = _tooltip[i];
        }

        if (Shell_NotifyIcon(_added ? NimModify : NimAdd, ref data))
        {
            _added = true;
        }
        else
        {
            FileLog.Warn("tray", "Shell_NotifyIcon add/modify failed");
        }
    }

    private static IntPtr LoadTrayIcon()
    {
        var path = Path.Combine(AppContext.BaseDirectory, "Assets", "AppIcon.ico");
        var h = LoadImage(IntPtr.Zero, path, ImageIcon, 0, 0, LrLoadFromFile | LrDefaultSize);
        if (h == IntPtr.Zero)
        {
            FileLog.Warn("tray", $"tray icon load failed: {path}");
        }

        return h;
    }

    [LibraryImport("shell32.dll", EntryPoint = "Shell_NotifyIconW", SetLastError = true)]
    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool Shell_NotifyIcon(uint message, ref NotifyIconData data);

    [LibraryImport("user32.dll", EntryPoint = "RegisterWindowMessageW",
        StringMarshalling = StringMarshalling.Utf16, SetLastError = true)]
    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    private static partial uint RegisterWindowMessage(string lpString);

    [LibraryImport("user32.dll", EntryPoint = "LoadImageW",
        StringMarshalling = StringMarshalling.Utf16, SetLastError = true)]
    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    private static partial IntPtr LoadImage(IntPtr hinst, string name, uint type, int cx, int cy, uint fuLoad);

    [LibraryImport("user32.dll", SetLastError = true)]
    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool DestroyIcon(IntPtr hIcon);

    [StructLayout(LayoutKind.Sequential)]
    private unsafe struct NotifyIconData
    {
        public uint CbSize;
        public IntPtr HWnd;
        public uint Uid;
        public uint UFlags;
        public uint UCallbackMessage;
        public IntPtr HIcon;
        public fixed char SzTip[128];
        public uint DwState;
        public uint DwStateMask;
        public fixed char SzInfo[256];
        public uint UTimeoutOrVersion;
        public fixed char SzInfoTitle[64];
        public uint DwInfoFlags;
        public Guid GuidItem;
        public IntPtr HBalloonIcon;
    }
}
