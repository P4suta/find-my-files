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

    [ObservableProperty]
    public partial bool IsOpen { get; set; }

    [ObservableProperty]
    public partial QueryTraceData? LastTrace { get; set; }

    [ObservableProperty]
    public partial EngineStatsData? Stats { get; set; }

    /// <summary>Latencies of the most recent queries (µs, oldest first).</summary>
    public IReadOnlyList<ulong> RecentTotalsUs => _recentTotalsUs;

    /// <summary>Raised on the UI thread whenever trace/stats data moved.</summary>
    public event Action? PerfDataChanged;

    public PerfPanelViewModel(IEngineClient engine)
    {
        _engine = engine;
    }

    public void Toggle() => IsOpen = !IsOpen;

    public async Task RefreshStatsAsync()
    {
        Stats = await _engine.GetStatsAsync();
        PerfDataChanged?.Invoke();
    }

    /// <summary>Record one completed query (trace may be null).</summary>
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
