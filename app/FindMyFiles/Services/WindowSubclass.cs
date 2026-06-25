using System.Runtime.CompilerServices;
using System.Runtime.InteropServices;

namespace FindMyFiles.Services;

/// <summary>
/// Subclasses a top-level window's HWND (comctl32 <c>SetWindowSubclass</c>) so
/// the app can observe raw Win32 messages — the tray icon's callback and the
/// <c>TaskbarCreated</c> broadcast (ADR-0030) — without replacing the window
/// procedure WinUI's <c>DesktopWindowXamlSource</c> relies on. Subclassing
/// <em>chains</em>, so XAML's own handling is preserved; this consumes only the
/// messages the callback claims and forwards everything else to
/// <c>DefSubclassProc</c>.
/// <para>The native callback is an <see cref="UnmanagedCallersOnlyAttribute"/>
/// static thunk reached by function pointer (the same pattern as
/// <c>NativeEngine.fmf_set_event_callback</c>), with the instance recovered from
/// a <see cref="GCHandle"/> passed as the subclass reference data — no managed
/// delegate is marshaled, so nothing can be GC-reclaimed out from under native
/// code.</para>
/// </summary>
internal sealed unsafe partial class WindowSubclass : IDisposable
{
    private readonly IntPtr _hwnd;
    private readonly IntPtr _id;
    private readonly Func<uint, IntPtr, IntPtr, bool> _onMessage;
    private GCHandle _self;
    private bool _disposed;

    /// <summary>Attaches a subclass to <paramref name="hwnd"/>.</summary>
    /// <param name="hwnd">The top-level window handle to subclass.</param>
    /// <param name="id">Subclass identity (unique per window), used to remove it.</param>
    /// <param name="onMessage">Raw message hook (uMsg, wParam, lParam); return
    /// true to consume the message, false to let default processing continue.</param>
    internal WindowSubclass(IntPtr hwnd, IntPtr id, Func<uint, IntPtr, IntPtr, bool> onMessage)
    {
        _hwnd = hwnd;
        _id = id;
        _onMessage = onMessage;
        _self = GCHandle.Alloc(this);
        if (!SetWindowSubclass(hwnd, &Thunk, id, GCHandle.ToIntPtr(_self)))
        {
            _self.Free();
            throw new InvalidOperationException("SetWindowSubclass failed");
        }
    }

    /// <summary>Removes the subclass and frees the instance handle. Idempotent.</summary>
    public void Dispose()
    {
        if (_disposed)
        {
            return;
        }

        _disposed = true;
        _ = RemoveWindowSubclass(_hwnd, &Thunk, _id);
        if (_self.IsAllocated)
        {
            _self.Free();
        }
    }

    [UnmanagedCallersOnly(CallConvs = [typeof(CallConvStdcall)])]
    private static IntPtr Thunk(
        IntPtr hWnd, uint uMsg, IntPtr wParam, IntPtr lParam, IntPtr uIdSubclass, IntPtr dwRefData)
    {
        if (GCHandle.FromIntPtr(dwRefData).Target is WindowSubclass self)
        {
            // A throwing handler must never propagate into native code (it would
            // crash the message pump). Swallow + log, then fall through to default.
            try
            {
                if (self._onMessage(uMsg, wParam, lParam))
                {
                    return IntPtr.Zero;
                }
            }
            catch (Exception ex)
            {
                FileLog.Warn("tray", $"window message handler failed: {ex.Message}");
            }
        }

        return DefSubclassProc(hWnd, uMsg, wParam, lParam);
    }

    [LibraryImport("comctl32.dll", SetLastError = true)]
    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool SetWindowSubclass(
        IntPtr hWnd,
        delegate* unmanaged[Stdcall]<IntPtr, uint, IntPtr, IntPtr, IntPtr, IntPtr, IntPtr> pfnSubclass,
        IntPtr uIdSubclass,
        IntPtr dwRefData);

    [LibraryImport("comctl32.dll", SetLastError = true)]
    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool RemoveWindowSubclass(
        IntPtr hWnd,
        delegate* unmanaged[Stdcall]<IntPtr, uint, IntPtr, IntPtr, IntPtr, IntPtr, IntPtr> pfnSubclass,
        IntPtr uIdSubclass);

    [LibraryImport("comctl32.dll")]
    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    private static partial IntPtr DefSubclassProc(IntPtr hWnd, uint uMsg, IntPtr wParam, IntPtr lParam);
}
