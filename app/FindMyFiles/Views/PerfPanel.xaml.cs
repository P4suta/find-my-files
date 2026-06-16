using System.ComponentModel;
using FindMyFiles.Engine;
using FindMyFiles.Services;
using FindMyFiles.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace FindMyFiles.Views;

/// <summary>
/// The F12 performance panel: stage bar (proportional theme-brush segments),
/// latency sparkline, volume/USN/error text blocks and the one-click
/// diagnostics dump. Rendered imperatively from
/// <see cref="PerfPanelViewModel.PerfDataChanged"/> — it is diagnostic
/// chrome, not app data. The host supplies <see cref="ViewModel"/> via
/// x:Bind; the 1 Hz stats poll runs only while the panel is open.
/// UI thread only.
/// </summary>
// View code-behind: imperative F12 rendering, not unit-tested (ADR-0022).
[System.Diagnostics.CodeAnalysis.ExcludeFromCodeCoverage]
public sealed partial class PerfPanel : UserControl
{
    /// <summary><see cref="ViewModel"/> のバッキング用 <c>DependencyProperty</c>。
    /// 値の差し替え時に `PerfDataChanged`/`PropertyChanged` の購読を旧→新へ張り替える。</summary>
    public static readonly DependencyProperty ViewModelProperty =
        DependencyProperty.Register(
            nameof(ViewModel),
            typeof(PerfPanelViewModel),
            typeof(PerfPanel),
            new PropertyMetadata(null, (d, e) => ((PerfPanel)d).OnViewModelChanged(e)));

    /// <summary>ホストが `x:Bind` で供給する診断 ViewModel。トレース/統計の更新通知元で、
    /// パネルを開いている間だけ 1 Hz の統計ポーリングを駆動する。</summary>
    public PerfPanelViewModel? ViewModel
    {
        get => (PerfPanelViewModel?)GetValue(ViewModelProperty);
        set => SetValue(ViewModelProperty, value);
    }

    private readonly Microsoft.UI.Dispatching.DispatcherQueueTimer _statsTimer;

    /// <summary>1 Hz の統計ポーリングタイマーを構築する(`ViewModel.IsOpen` が真の間だけ
    /// 走らせる)。タイマー自体はここでは開始せず、開閉に応じて起動/停止する。</summary>
    public PerfPanel()
    {
        InitializeComponent();
        _statsTimer = App.DispatcherQueue.CreateTimer();
        _statsTimer.Interval = TimeSpan.FromSeconds(1);
        _statsTimer.Tick += (_, _) =>
        {
            if (ViewModel is { } vm)
            {
                vm.RefreshStatsAsync().Forget("perf.stats");
            }
        };
    }

    private void OnViewModelChanged(DependencyPropertyChangedEventArgs e)
    {
        if (e.OldValue is PerfPanelViewModel old)
        {
            old.PerfDataChanged -= RenderPerf;
            old.PropertyChanged -= OnViewModelPropertyChanged;
        }

        if (e.NewValue is PerfPanelViewModel vm)
        {
            vm.PerfDataChanged += RenderPerf;
            vm.PropertyChanged += OnViewModelPropertyChanged;
        }
    }

    private void OnViewModelPropertyChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (string.Equals(e.PropertyName, nameof(PerfPanelViewModel.IsOpen), StringComparison.Ordinal) && ViewModel is { } vm)
        {
            if (vm.IsOpen)
            {
                _statsTimer.Start();
                vm.RefreshStatsAsync().Forget("perf.stats");
            }
            else
            {
                _statsTimer.Stop();
            }
        }
    }

    /// <summary>
    /// One-click bug-report payload: engine stats JSON + app log tail +
    /// environment. The dev-side half of「黙らない」.
    /// </summary>
    private void CopyDiag_Click(object sender, Microsoft.UI.Xaml.RoutedEventArgs e)
    {
        var statsJson = ViewModel?.Stats is { } s
            ? System.Text.Json.JsonSerializer.Serialize(s, DiagJsonOptions)
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

    /// <summary>
    /// Diagnostic chrome rendered imperatively: stage bar (proportional
    /// theme-brush segments), latency sparkline, volume/USN text blocks.
    /// </summary>
    private void RenderPerf()
    {
        if (ViewModel is not { IsOpen: true } vm)
        {
            return;
        }

        var t = vm.LastTrace;
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
        var recent = vm.RecentTotalsUs;
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
                    H - ((recent[i] / (double)max) * (H - 2)) - 1));
            }

            Spark.Points = points;
        }

        var stats = vm.Stats;
        if (stats is not null)
        {
            HistText.Text = $"p50 {stats.P50Us / 1000.0:F2}ms   p99 {stats.P99Us / 1000.0:F2}ms";
            TransportText.Text = $"Engine: {vm.EngineMode}"
                + (stats.Transport is { } tr
                    ? $"\nTransport: {tr.State} / reconnects {tr.Reconnects}"
                        + $" / page RTT EWMA {tr.PageRttEwmaUs:F0}µs / server pid {tr.ServerPid}"
                    : string.Empty);
            VolumesText.Text = string.Join("\n", stats.Indexes.Select(v =>
                $"{v.Volume}  {v.LiveEntries:N0} 件  {v.TotalBytes / (1024.0 * 1024.0):F0} MB" +
                $"  ({v.BytesPerEntry:F0} B/件)  gen {v.ContentGeneration}"));
            UsnFeed.Text = string.Join("\n", stats.RecentUsn.TakeLast(6).Select(u =>
                $"{u.Volume} {u.Records}rec → +{u.Upserted} -{u.Deleted} ~{u.StatUpdated}" +
                $" ({u.ApplyUs}µs)"));

            // Degradations: recent WARN+/panic events and nonzero counters.
            ErrorsText.Text = string.Join("\n", stats.RecentErrors.TakeLast(8).Select(er =>
                $"[{er.UptimeMs / 1000}s] {er.Severity.ToUpperInvariant()} {er.Area}" +
                $"{(string.IsNullOrEmpty(er.Volume) ? string.Empty : $" ({er.Volume})")}: " +
                $"{FirstLine(er.Message)}"));

            // Reflect over the generated CountersData (EngineContract.g.cs)
            // so a counter added to the contract registry shows up here with
            // zero UI edits — the hand-written list this replaced silently
            // missed 8 of 18 counters.
            var c = stats.Counters;
            var nonzero = CounterProps
                .Select(p => (
                    Name: System.Text.Json.JsonNamingPolicy.SnakeCaseLower.ConvertName(p.Name),
                    V: (ulong)p.GetValue(c)!))
                .Where(x => x.V > 0)
                .ToList();
            CountersText.Text = nonzero.Count == 0
                ? string.Empty
                : "劣化カウンタ: " + string.Join("  ", nonzero.Select(x => $"{x.Name}={x.V}"));
        }
    }

    private static readonly System.Text.Json.JsonSerializerOptions DiagJsonOptions =
        new(Engine.EngineJson.SnakeCase)
        {
            WriteIndented = true,
        };

    private static readonly System.Reflection.PropertyInfo[] CounterProps =
        [.. typeof(CountersData).GetProperties()
            .Where(p => p.PropertyType == typeof(ulong))];

    private static string FirstLine(string s)
    {
        var i = s.IndexOf('\n', StringComparison.Ordinal);
        var line = i < 0 ? s : s[..i];
        return line.Length > 120 ? line[..120] + "…" : line;
    }
}
