using System.ComponentModel;
using FindMyFiles.Services;
using FindMyFiles.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace FindMyFiles.Views;

/// <summary>
/// The F12 diagnostics panel. The stage bar (proportional theme-brush
/// segments) and the latency sparkline are drawn imperatively from
/// <see cref="PerfPanelViewModel.PerfDataChanged"/> — diagnostic chrome, not
/// app data. Everything else (headline, volumes, USN, transport, errors,
/// counters) is declarative x:Bind through DiagFormat. The host supplies
/// <see cref="ViewModel"/> via x:Bind; the 1 Hz stats poll runs only while the
/// panel is open. UI thread only.
/// </summary>
// View code-behind: imperative F12 rendering, not unit-tested (ADR-0022).
[System.Diagnostics.CodeAnalysis.ExcludeFromCodeCoverage]
public sealed partial class PerfPanel : UserControl
{
    /// <summary>Backing <c>DependencyProperty</c> for <see cref="ViewModel"/>.
    /// On value swap, re-routes `PerfDataChanged`/`PropertyChanged` subscriptions from old to new.</summary>
    public static readonly DependencyProperty ViewModelProperty =
        DependencyProperty.Register(
            nameof(ViewModel),
            typeof(PerfPanelViewModel),
            typeof(PerfPanel),
            new PropertyMetadata(null, (d, e) => ((PerfPanel)d).OnViewModelChanged(e)));

    /// <summary>Diagnostic ViewModel supplied by the host via `x:Bind`. Source of
    /// trace/stats update notifications; drives the 1 Hz stats poll only while the panel is open.</summary>
    public PerfPanelViewModel? ViewModel
    {
        get => (PerfPanelViewModel?)GetValue(ViewModelProperty);
        set => SetValue(ViewModelProperty, value);
    }

    private readonly Microsoft.UI.Dispatching.DispatcherQueueTimer _statsTimer;

    /// <summary>Builds the 1 Hz stats poll timer (runs only while `ViewModel.IsOpen` is true).
    /// The timer is not started here; it is started/stopped on open/close.</summary>
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
            _statsTimer.Stop();
        }

        if (e.NewValue is PerfPanelViewModel vm)
        {
            vm.PerfDataChanged += RenderPerf;
            vm.PropertyChanged += OnViewModelPropertyChanged;
            SyncTimer(vm);
        }
    }

    private void OnViewModelPropertyChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (string.Equals(e.PropertyName, nameof(PerfPanelViewModel.IsOpen), StringComparison.Ordinal) && ViewModel is { } vm)
        {
            SyncTimer(vm);
        }
    }

    /// <summary>
    /// Reconciles the 1 Hz stats poll with <see cref="PerfPanelViewModel.IsOpen"/>.
    /// Called both on `ViewModel` assignment and on `IsOpen` change so the poll
    /// starts regardless of whether the DP is set before or after the panel opens
    /// (<c>Start</c> is idempotent). An immediate refresh primes the first frame.
    /// </summary>
    private void SyncTimer(PerfPanelViewModel vm)
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

    /// <summary>
    /// One-click bug-report payload: engine stats JSON + app log tail +
    /// environment. The dev-side half of "don't go silent".
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
    /// Diagnostic chrome rendered imperatively: the stage bar (proportional
    /// theme-brush segments) and the latency sparkline. All textual data is
    /// declarative x:Bind, not touched here.
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
    }

    private static readonly System.Text.Json.JsonSerializerOptions DiagJsonOptions =
        new(Engine.EngineJson.SnakeCase)
        {
            WriteIndented = true,
        };
}
