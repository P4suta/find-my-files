using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Windows.System;
using FindMyFiles.Engine;
using FindMyFiles.Services;
using FindMyFiles.ViewModels;

namespace FindMyFiles;

public sealed partial class MainPage : Page
{
    public MainViewModel ViewModel { get; }

    private readonly Microsoft.UI.Dispatching.DispatcherQueueTimer _statsTimer;

    public MainPage()
    {
        ViewModel = new MainViewModel(
            App.EngineClient, new DispatcherQueueDispatcher(App.DispatcherQueue));
        InitializeComponent();
        _statsTimer = App.DispatcherQueue.CreateTimer();
        _statsTimer.Interval = TimeSpan.FromSeconds(1);
        _statsTimer.Tick += (_, _) => ViewModel.Perf.RefreshStatsAsync().Forget("perf.stats");
        ViewModel.Perf.PerfDataChanged += RenderPerf;
        ViewModel.Perf.PropertyChanged += (_, e) =>
        {
            if (e.PropertyName == nameof(ViewModel.Perf.IsOpen))
            {
                if (ViewModel.Perf.IsOpen)
                {
                    _statsTimer.Start();
                    ViewModel.Perf.RefreshStatsAsync().Forget("perf.stats");
                }
                else
                {
                    _statsTimer.Stop();
                }
            }
        };
        ViewModel.Results.ResultsPublished += OnResultsPublished;
        ResultsList.SelectionChanged += (_, _) =>
        {
            // Remember the last real selection so a position-preserving
            // requery can re-find it (Reset clears the ListView selection).
            if (ResultsList.SelectedItem is ResultRow { IsPlaceholder: false } row)
            {
                _lastSelectedEntryRef = row.EntryRef;
            }
        };
        Loaded += (_, _) =>
        {
            SearchBox.Focus(Microsoft.UI.Xaml.FocusState.Programmatic);
            ViewModel.Start();
        };
    }

    // ── Viewport placement after each published result ──────────────────

    private ScrollViewer? _resultsScroller;
    private ulong? _lastSelectedEntryRef;

    /// <summary>
    /// Reset origins (typing, sort…) land at the top; position-preserving
    /// origins (index changed, stale…) restore the previous first visible row
    /// and, best effort, the selection. Explicit placement — the ListView's
    /// own behavior after a Reset is version-dependent.
    /// </summary>
    private void OnResultsPublished(ResultsPublication pub)
    {
        if (pub.RestoreIndex is { } restore && restore < ResultsList.Items.Count)
        {
            ResultsList.ScrollIntoView(
                ResultsList.Items[restore], ScrollIntoViewAlignment.Leading);
            RestoreSelection(pub);
        }
        else
        {
            _resultsScroller ??= FindScrollViewer(ResultsList);
            _resultsScroller?.ChangeView(null, 0, null, disableAnimation: true);
        }
    }

    private void RestoreSelection(ResultsPublication pub)
    {
        if (_lastSelectedEntryRef is not { } entryRef)
        {
            return;
        }
        for (var i = pub.FirstSeededIndex;
             i <= pub.LastSeededIndex && i < ResultsList.Items.Count;
             i++)
        {
            if (ResultsList.Items[i] is ResultRow { IsPlaceholder: false } row
                && row.EntryRef == entryRef)
            {
                ResultsList.SelectedIndex = i;
                return;
            }
        }
    }

    private static ScrollViewer? FindScrollViewer(Microsoft.UI.Xaml.DependencyObject root)
    {
        for (var i = 0; i < Microsoft.UI.Xaml.Media.VisualTreeHelper.GetChildrenCount(root); i++)
        {
            var child = Microsoft.UI.Xaml.Media.VisualTreeHelper.GetChild(root, i);
            if (child is ScrollViewer viewer)
            {
                return viewer;
            }
            if (FindScrollViewer(child) is { } nested)
            {
                return nested;
            }
        }
        return null;
    }

    public Microsoft.UI.Xaml.Visibility BoolToVis(bool value) =>
        value ? Microsoft.UI.Xaml.Visibility.Visible : Microsoft.UI.Xaml.Visibility.Collapsed;

    public static InfoBarSeverity ToInfoSeverity(NotifySeverity s) => s switch
    {
        NotifySeverity.Error => InfoBarSeverity.Error,
        NotifySeverity.Warning => InfoBarSeverity.Warning,
        _ => InfoBarSeverity.Informational,
    };

    private void Notification_Closed(InfoBar sender, InfoBarClosedEventArgs args)
    {
        if (sender.DataContext is AppNotification n)
        {
            ViewModel.Notifications.Remove(n);
        }
    }

    /// <summary>
    /// One-click bug-report payload: engine stats JSON + app log tail +
    /// environment. The dev-side half of「黙らない」.
    /// </summary>
    private void CopyDiag_Click(object sender, Microsoft.UI.Xaml.RoutedEventArgs e)
    {
        var statsJson = ViewModel.Perf.Stats is { } s
            ? System.Text.Json.JsonSerializer.Serialize(s, new System.Text.Json.JsonSerializerOptions
            {
                WriteIndented = true,
                PropertyNamingPolicy = System.Text.Json.JsonNamingPolicy.SnakeCaseLower,
            })
            : "(no stats yet)";
        var dump =
            $"find-my-files diagnostics {DateTimeOffset.Now:O}\n" +
            $"app: v{typeof(App).Assembly.GetName().Version}  os: {Environment.OSVersion.VersionString}\n" +
            $"engine log: %ProgramData%\\find-my-files\\logs\\engine.log\n" +
            $"app log: {FileLog.LogPath}\n\n=== engine stats ===\n{statsJson}\n\n" +
            $"=== app.log (tail) ===\n{FileLog.Tail(50)}\n";
        ShellOps.CopyText(dump, "diagnostics");
        Notifier.Post(NotifySeverity.Info, "診断情報をクリップボードにコピーしました");
    }

    private void PerfPanel_Toggle(
        Microsoft.UI.Xaml.Input.KeyboardAccelerator sender,
        Microsoft.UI.Xaml.Input.KeyboardAcceleratorInvokedEventArgs args)
    {
        ViewModel.Perf.Toggle();
        args.Handled = true;
    }

    /// <summary>
    /// Diagnostic chrome rendered imperatively: stage bar (proportional
    /// theme-brush segments), latency sparkline, volume/USN text blocks.
    /// </summary>
    private void RenderPerf()
    {
        if (!ViewModel.Perf.IsOpen)
        {
            return;
        }

        var t = ViewModel.Perf.LastTrace;
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
        var recent = ViewModel.Perf.RecentTotalsUs;
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

        var stats = ViewModel.Perf.Stats;
        if (stats is not null)
        {
            HistText.Text = $"p50 {stats.P50Us / 1000.0:F2}ms   p99 {stats.P99Us / 1000.0:F2}ms";
            VolumesText.Text = string.Join("\n", stats.Indexes.Select(v =>
                $"{v.Volume}  {v.LiveEntries:N0} 件  {v.TotalBytes / (1024.0 * 1024.0):F0} MB" +
                $"  ({v.BytesPerEntry:F0} B/件)  gen {v.ContentGeneration}"));
            UsnFeed.Text = string.Join("\n", stats.RecentUsn.TakeLast(6).Select(u =>
                $"{u.Volume} {u.Records}rec → +{u.Upserted} -{u.Deleted} ~{u.StatUpdated}" +
                $" ({u.ApplyUs}µs)"));

            // Degradations: recent WARN+/panic events and nonzero counters.
            ErrorsText.Text = string.Join("\n", stats.RecentErrors.TakeLast(8).Select(er =>
                $"[{er.UptimeMs / 1000}s] {er.Severity.ToUpperInvariant()} {er.Area}" +
                $"{(string.IsNullOrEmpty(er.Volume) ? "" : $" ({er.Volume})")}: " +
                $"{FirstLine(er.Message)}"));
            var c = stats.Counters;
            var nonzero = new (string Name, ulong V)[]
            {
                ("stat_fetch_failures", c.StatFetchFailures),
                ("usn_batches_truncated", c.UsnBatchesTruncated),
                ("snapshot_load_failures", c.SnapshotLoadFailures),
                ("snapshot_save_failures", c.SnapshotSaveFailures),
                ("deferred_names_unresolved", c.DeferredNamesUnresolved),
                ("corrupt_mft_records", c.CorruptMftRecords),
                ("journal_rescans", c.JournalRescans),
            }.Where(x => x.V > 0).ToList();
            CountersText.Text = nonzero.Count == 0
                ? string.Empty
                : "劣化カウンタ: " + string.Join("  ", nonzero.Select(x => $"{x.Name}={x.V}"));
        }
    }

    private static string FirstLine(string s)
    {
        var i = s.IndexOf('\n');
        var line = i < 0 ? s : s[..i];
        return line.Length > 120 ? line[..120] + "…" : line;
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

    private static void OpenRow(ResultRow? row)
    {
        if (row is { IsPlaceholder: false })
        {
            ShellOps.Open(row.FullPath);
        }
    }

    private static void RevealRow(ResultRow? row)
    {
        if (row is { IsPlaceholder: false })
        {
            ShellOps.Reveal(row.FullPath);
        }
    }

    private void CopySelectedPaths()
    {
        var paths = ResultsList.SelectedItems
            .OfType<ResultRow>()
            .Where(r => !r.IsPlaceholder)
            .Select(r => r.FullPath)
            .ToList();
        if (paths.Count > 0)
        {
            ShellOps.CopyText(string.Join("\r\n", paths), "paths");
        }
    }

    private void HeaderName_Click(object sender, Microsoft.UI.Xaml.RoutedEventArgs e) =>
        ViewModel.SetSort(FmfSort.Name);

    private void HeaderSize_Click(object sender, Microsoft.UI.Xaml.RoutedEventArgs e) =>
        ViewModel.SetSort(FmfSort.Size);

    private void HeaderDate_Click(object sender, Microsoft.UI.Xaml.RoutedEventArgs e) =>
        ViewModel.SetSort(FmfSort.Mtime);
}
