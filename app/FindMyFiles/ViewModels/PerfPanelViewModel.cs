using CommunityToolkit.Mvvm.ComponentModel;
using FindMyFiles.Engine;

namespace FindMyFiles.ViewModels;

/// <summary>
/// State behind the F12 performance panel: the last query trace, the engine
/// stats snapshot and the recent-latency history. Rendering stays in
/// code-behind (diagnostic chrome, not app data).
/// </summary>
public sealed partial class PerfPanelViewModel : ObservableObject
{
    private const int MaxRecent = 64;

    private readonly IEngineClient _engine;
    private readonly List<ulong> _recentTotalsUs = [];

    /// <summary>Whether the F12 panel is showing (toggled by <see cref="Toggle"/>).</summary>
    [ObservableProperty]
    public partial bool IsOpen { get; set; }

    /// <summary>Stage breakdown of the most recent query, or null when the
    /// engine emitted no trace (e.g. an empty query). Fed by <see cref="RecordTrace"/>.</summary>
    [ObservableProperty]
    public partial QueryTraceData? LastTrace { get; set; }

    /// <summary>Last engine stats snapshot (counters, RAM, recent errors), or
    /// null before the first <see cref="RefreshStatsAsync"/>.</summary>
    [ObservableProperty]
    public partial EngineStatsData? Stats { get; set; }

    /// <summary>Latencies of the most recent queries (µs, oldest first).</summary>
    public IReadOnlyList<ulong> RecentTotalsUs => _recentTotalsUs;

    /// <summary>Engine transport label for the F12 panel — moved off the gear
    /// menu, where its internal terms (fake / in-proc) confused end users; F12
    /// is diagnostic, so the precise vocabulary stays here.</summary>
    public string EngineMode => StatusFormatter.EngineMode(_engine);

    /// <summary>Raised on the UI thread whenever trace/stats data moved.</summary>
    public event Action? PerfDataChanged;

    /// <summary>Binds the panel to <paramref name="engine"/> — the source of
    /// both the stats snapshot and the transport label.</summary>
    /// <param name="engine">Engine client supplying stats and the transport label.</param>
    public PerfPanelViewModel(IEngineClient engine)
    {
        _engine = engine;
    }

    /// <summary>Flip the panel's visibility (the F12 shortcut / debug menu).</summary>
    public void Toggle() => IsOpen = !IsOpen;

    /// <summary>Pull a fresh <see cref="Stats"/> snapshot from the engine and
    /// raise <see cref="PerfDataChanged"/>. Awaitable so a pipe round-trip
    /// doesn't block the caller.</summary>
    /// <returns>A <see cref="Task"/> that completes once the snapshot is refreshed.</returns>
    public async Task RefreshStatsAsync()
    {
        Stats = await _engine.GetStatsAsync().ConfigureAwait(false);
        PerfDataChanged?.Invoke();
    }

    /// <summary>Record one completed query (trace may be null).</summary>
    /// <param name="trace">Stage breakdown of the query, or null when none was emitted.</param>
    public void RecordTrace(QueryTraceData? trace)
    {
        LastTrace = trace;
        if (trace is { } t)
        {
            _recentTotalsUs.Add(t.TotalUs);
            if (_recentTotalsUs.Count > MaxRecent)
            {
                _recentTotalsUs.RemoveAt(0);
            }
        }

        PerfDataChanged?.Invoke();
    }
}
