using System.Runtime.InteropServices;
using FindMyFiles.Services;
using FindMyFiles.ViewModels;
using Microsoft.UI;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Windows.Graphics;

namespace FindMyFiles.Views;

/// <summary>The diagnostics window: a non-modal utility window hosting the
/// <see cref="PerfPanel"/>. Shares the single <see cref="PerfPanelViewModel"/>
/// owned by the main view, so trace recording and the 1 Hz live poll keep
/// running regardless of where the panel is shown.</summary>
// View shell: window chrome + title bar, not unit-tested (ADR-0022).
[System.Diagnostics.CodeAnalysis.ExcludeFromCodeCoverage]
public sealed partial class DiagnosticsWindow : Window
{
    /// <summary>Extends the title bar into the content, sets the window icon and
    /// title, hands the shared <paramref name="perf"/> ViewModel to the hosted
    /// <see cref="PerfPanel"/>, then sizes (DPI-correct, before first paint) and
    /// centers the window on its display.</summary>
    /// <param name="perf">The shared diagnostics ViewModel owned by the main view.</param>
    public DiagnosticsWindow(PerfPanelViewModel perf)
    {
        InitializeComponent();
        Title = Loc.Get("DiagWindowTitle");
        DiagTitleBar.Title = Loc.Get("DiagWindowTitle");
        ExtendsContentIntoTitleBar = true;
        SetTitleBar(DiagTitleBar);
        AppWindow.SetIcon("Assets/AppIcon.ico");
        DiagPerfPanel.ViewModel = perf;

        // DPI-correct, flicker-free sizing (ctor runs before first paint).
        var hwnd = Win32Interop.GetWindowFromWindowId(AppWindow.Id);
        var scale = GetDpiForWindow(hwnd) / 96.0;
        AppWindow.ResizeClient(new SizeInt32((int)(560 * scale), (int)(720 * scale)));
        var area = DisplayArea.GetFromWindowId(AppWindow.Id, DisplayAreaFallback.Nearest).WorkArea;
        var sz = AppWindow.Size;
        AppWindow.Move(new PointInt32(
            area.X + ((area.Width - sz.Width) / 2),
            area.Y + ((area.Height - sz.Height) / 2)));
        if (AppWindow.Presenter is OverlappedPresenter p)
        {
            p.PreferredMinimumWidth = 440;
            p.PreferredMinimumHeight = 540;
            p.IsMaximizable = false;
        }
    }

    [DllImport("user32.dll")]
    private static extern uint GetDpiForWindow(IntPtr hWnd);
}
