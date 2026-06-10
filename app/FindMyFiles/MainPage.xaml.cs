using System.Diagnostics;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Windows.ApplicationModel.DataTransfer;
using Windows.System;
using FindMyFiles.Engine;
using FindMyFiles.ViewModels;

namespace FindMyFiles;

public sealed partial class MainPage : Page
{
    public MainViewModel ViewModel { get; }

    private readonly Microsoft.UI.Dispatching.DispatcherQueueTimer _statsTimer;

    public MainPage()
    {
        ViewModel = new MainViewModel(App.EngineClient, App.DispatcherQueue);
        InitializeComponent();
        _statsTimer = App.DispatcherQueue.CreateTimer();
        _statsTimer.Interval = TimeSpan.FromSeconds(1);
        _statsTimer.Tick += (_, _) => _ = ViewModel.RefreshStatsAsync();
        ViewModel.PerfDataChanged += RenderPerf;
        ViewModel.PropertyChanged += (_, e) =>
        {
            if (e.PropertyName == nameof(ViewModel.IsPerfPanelOpen))
            {
                if (ViewModel.IsPerfPanelOpen)
                {
                    _statsTimer.Start();
                    _ = ViewModel.RefreshStatsAsync();
                }
                else
                {
                    _statsTimer.Stop();
                }
            }
        };
        Loaded += (_, _) =>
        {
            SearchBox.Focus(Microsoft.UI.Xaml.FocusState.Programmatic);
            ViewModel.Start();
        };
    }

    public Microsoft.UI.Xaml.Visibility BoolToVis(bool value) =>
        value ? Microsoft.UI.Xaml.Visibility.Visible : Microsoft.UI.Xaml.Visibility.Collapsed;

    private void PerfPanel_Toggle(
        Microsoft.UI.Xaml.Input.KeyboardAccelerator sender,
        Microsoft.UI.Xaml.Input.KeyboardAcceleratorInvokedEventArgs args)
    {
        ViewModel.TogglePerfPanel();
        args.Handled = true;
    }

    /// <summary>
    /// Diagnostic chrome rendered imperatively: stage bar (proportional
    /// theme-brush segments), latency sparkline, volume/USN text blocks.
    /// </summary>
    private void RenderPerf()
    {
        if (!ViewModel.IsPerfPanelOpen)
        {
            return;
        }

        var t = ViewModel.LastTrace;
        if (t is not null)
        {
            PerfHeadline.Text =
                $"{t.Query switch { "" => "(all)", var q => q }}  —  {t.TotalUs / 1000.0:F2} ms" +
                $"  driver={t.Driver}  hits={t.Hits:N0}  scanned={t.EntriesScanned:N0}";

            (string Name, ulong Us, string BrushKey)[] stages =
            [
                ("parse", t.ParseUs + t.CompileUs, "AccentFillColorTertiaryBrush"),
                ("memo", t.MemoUs, "SystemFillColorCautionBrush"),
                ("scan", t.ScanUs, "AccentFillColorDefaultBrush"),
                ("mat", t.MaterializeUs, "SystemFillColorSuccessBrush"),
                ("merge", t.MergeUs, "SystemFillColorNeutralBrush"),
            ];
            StageBar.ColumnDefinitions.Clear();
            StageBar.Children.Clear();
            var col = 0;
            foreach (var (_, us, brushKey) in stages)
            {
                if (us == 0)
                {
                    continue;
                }
                StageBar.ColumnDefinitions.Add(new ColumnDefinition
                {
                    Width = new Microsoft.UI.Xaml.GridLength(us, Microsoft.UI.Xaml.GridUnitType.Star),
                });
                var seg = new Microsoft.UI.Xaml.Controls.Border
                {
                    Background =
                        (Microsoft.UI.Xaml.Media.Brush)Microsoft.UI.Xaml.Application.Current
                            .Resources[brushKey],
                    Margin = new Microsoft.UI.Xaml.Thickness(0, 0, 1, 0),
                    CornerRadius = new Microsoft.UI.Xaml.CornerRadius(2),
                };
                Microsoft.UI.Xaml.Controls.Grid.SetColumn(seg, col++);
                StageBar.Children.Add(seg);
            }
            StageLegend.Text = string.Join("  ", stages
                .Where(s => s.Us > 0)
                .Select(s => $"{s.Name} {s.Us / 1000.0:F2}ms"));
        }

        // Sparkline over the recent query latencies.
        var recent = ViewModel.RecentTotalsUs;
        if (recent.Count >= 2)
        {
            var w = Math.Max(Spark.ActualWidth, 240.0);
            const double H = 40.0;
            var max = Math.Max(recent.Max(), 1UL);
            var points = new Microsoft.UI.Xaml.Media.PointCollection();
            for (var i = 0; i < recent.Count; i++)
            {
                points.Add(new Windows.Foundation.Point(
                    i * w / Math.Max(recent.Count - 1, 1),
                    H - (recent[i] / (double)max) * (H - 2) - 1));
            }
            Spark.Points = points;
        }

        var stats = ViewModel.Stats;
        if (stats is not null)
        {
            HistText.Text = $"p50 {stats.P50Us / 1000.0:F2}ms   p99 {stats.P99Us / 1000.0:F2}ms";
            VolumesText.Text = string.Join("\n", stats.Indexes.Select(v =>
                $"{v.Volume}  {v.LiveEntries:N0} 件  {v.TotalBytes / (1024.0 * 1024.0):F0} MB" +
                $"  ({v.BytesPerEntry:F0} B/件)  gen {v.ContentGeneration}"));
            UsnFeed.Text = string.Join("\n", stats.RecentUsn.TakeLast(6).Select(u =>
                $"{u.Volume} {u.Records}rec → +{u.Upserted} -{u.Deleted} ~{u.StatUpdated}" +
                $" ({u.ApplyUs}µs)"));
        }
    }

    private void SearchBox_KeyDown(object sender, KeyRoutedEventArgs e)
    {
        switch (e.Key)
        {
            case VirtualKey.Down:
                ResultsList.Focus(Microsoft.UI.Xaml.FocusState.Programmatic);
                if (ResultsList.Items.Count > 0)
                {
                    ResultsList.SelectedIndex = 0;
                    ResultsList.ScrollIntoView(ResultsList.SelectedItem);
                }
                e.Handled = true;
                break;
            case VirtualKey.Enter:
                OpenRow(FirstSelectedOrTop());
                e.Handled = true;
                break;
            case VirtualKey.Escape:
                ViewModel.SearchText = string.Empty;
                e.Handled = true;
                break;
        }
    }

    private void ResultsList_KeyDown(object sender, KeyRoutedEventArgs e)
    {
        var ctrl = (Microsoft.UI.Input.InputKeyboardSource
            .GetKeyStateForCurrentThread(VirtualKey.Control)
            & Windows.UI.Core.CoreVirtualKeyStates.Down) != 0;
        switch (e.Key)
        {
            case VirtualKey.Enter when ctrl:
                RevealRow(SelectedRow());
                e.Handled = true;
                break;
            case VirtualKey.Enter:
                OpenRow(SelectedRow());
                e.Handled = true;
                break;
            case VirtualKey.C when ctrl:
                CopySelectedPaths();
                e.Handled = true;
                break;
            case VirtualKey.Escape:
                SearchBox.Focus(Microsoft.UI.Xaml.FocusState.Programmatic);
                SearchBox.SelectAll();
                e.Handled = true;
                break;
        }
    }

    private ResultRow? SelectedRow() => ResultsList.SelectedItem as ResultRow;

    private ResultRow? FirstSelectedOrTop() =>
        SelectedRow() ?? (ResultsList.Items.Count > 0 ? ResultsList.Items[0] as ResultRow : null);

    private void ResultsList_DoubleTapped(object sender, DoubleTappedRoutedEventArgs e) =>
        OpenRow(SelectedRow());

    private void MenuOpen_Click(object sender, Microsoft.UI.Xaml.RoutedEventArgs e) =>
        OpenRow(SelectedRow());

    private void MenuOpenPath_Click(object sender, Microsoft.UI.Xaml.RoutedEventArgs e) =>
        RevealRow(SelectedRow());

    private void MenuCopyPath_Click(object sender, Microsoft.UI.Xaml.RoutedEventArgs e) =>
        CopySelectedPaths();

    /// <summary>
    /// Open via explorer.exe so the target launches *unelevated* — launching
    /// directly from this admin process would elevate the associated app
    /// (CLAUDE.md UI固定則).
    /// </summary>
    private static void OpenRow(ResultRow? row)
    {
        if (row is null || row.IsPlaceholder)
        {
            return;
        }
        Process.Start(new ProcessStartInfo
        {
            FileName = "explorer.exe",
            Arguments = $"\"{row.FullPath}\"",
            UseShellExecute = false,
        });
    }

    private static void RevealRow(ResultRow? row)
    {
        if (row is null || row.IsPlaceholder)
        {
            return;
        }
        Process.Start(new ProcessStartInfo
        {
            FileName = "explorer.exe",
            Arguments = $"/select,\"{row.FullPath}\"",
            UseShellExecute = false,
        });
    }

    private void CopySelectedPaths()
    {
        var paths = ResultsList.SelectedItems
            .OfType<ResultRow>()
            .Where(r => !r.IsPlaceholder)
            .Select(r => r.FullPath)
            .ToList();
        if (paths.Count == 0)
        {
            return;
        }
        var pkg = new DataPackage();
        pkg.SetText(string.Join("\r\n", paths));
        Clipboard.SetContent(pkg);
    }

    private void HeaderName_Click(object sender, Microsoft.UI.Xaml.RoutedEventArgs e) =>
        ViewModel.SetSort(FmfSort.Name);

    private void HeaderSize_Click(object sender, Microsoft.UI.Xaml.RoutedEventArgs e) =>
        ViewModel.SetSort(FmfSort.Size);

    private void HeaderDate_Click(object sender, Microsoft.UI.Xaml.RoutedEventArgs e) =>
        ViewModel.SetSort(FmfSort.Mtime);
}
