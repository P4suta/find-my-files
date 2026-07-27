using CommunityToolkit.Mvvm.ComponentModel;
using FindMyFiles.Engine;
using FindMyFiles.Services;

namespace FindMyFiles.ViewModels;

/// <summary>
/// State behind the F12 performance panel: the last query trace, the engine
/// stats snapshot and the recent-latency history. Rendering stays in
/// code-behind (diagnostic chrome, not app data).
/// </summary>
internal sealed partial class PerfPanelViewModel : ObservableObject, IDisposable
{
    private const int MaxRecent = 64;
    private const int UsnTailMax = 6;
    private const int ErrorTailMax = 8;
    private const int ScanTailMax = 4;

    private readonly IEngineClient _engine;
    private readonly CancellationTokenSource _lifetime = new();
    private readonly List<ulong> _recentTotalsUs = [];
    private readonly List<ulong> _recentWsBytes = [];
    private int _disposed;

    /// <summary>Type name of the failure the last stats poll hit, or null when
    /// the last poll succeeded. Only a <em>change</em> of kind is logged, so a
    /// permanently failing engine costs one line, not one per second.</summary>
    private string? _statsFailureKind;

    /// <summary>Consecutive failed stats polls since the last success — the
    /// scale of the outage, carried on the log lines that bracket it.</summary>
    private int _statsFailures;

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
    [NotifyPropertyChangedFor(nameof(RecentUsnTail))]
    [NotifyPropertyChangedFor(nameof(RecentErrorsTail))]
    [NotifyPropertyChangedFor(nameof(ScansTail))]
    public partial EngineStatsData? Stats { get; set; }

    /// <summary>Latencies of the most recent queries (µs, oldest first).</summary>
    public IReadOnlyList<ulong> RecentTotalsUs => _recentTotalsUs;

    /// <summary>Host-process working set over the recent polls (bytes, oldest
    /// first) — the source for the memory sparkline.</summary>
    public IReadOnlyList<ulong> RecentWsBytes => _recentWsBytes;

    /// <summary>The most recent index-establish events (scan/snapshot restore,
    /// capped, newest last) for the scan card's feed; re-notifies when
    /// <see cref="Stats"/> swaps.</summary>
    public IReadOnlyList<ScanTraceData> ScansTail =>
        Stats?.Scans is { } s ? s.TakeLast(ScanTailMax).ToList() : [];

    /// <summary>The most recent USN batches (capped) for the panel's storage
    /// card. x:Bind can't call <c>TakeLast</c>, so the cap lives here; it
    /// re-notifies when <see cref="Stats"/> swaps.</summary>
    public IReadOnlyList<UsnTraceData> RecentUsnTail =>
        Stats?.RecentUsn is { } u ? u.TakeLast(UsnTailMax).ToList() : [];

    /// <summary>The most recent WARN+ engine events (capped) for the panel's
    /// health card; re-notifies when <see cref="Stats"/> swaps.</summary>
    public IReadOnlyList<ErrorEventData> RecentErrorsTail =>
        Stats?.RecentErrors is { } e ? e.TakeLast(ErrorTailMax).ToList() : [];

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
    /// doesn't block the caller.
    /// <para>Best-effort by contract: the panel polls at 1 Hz, so a failure that
    /// escapes here is not one error but an error notification <em>every second</em>
    /// (the caller fire-and-forgets through <c>Forget</c>, which posts to the
    /// notification bar). Not being able to read diagnostics is a diagnostic gap,
    /// not an application fault — every failure is contained and logged instead,
    /// deduplicated by kind so a persistent outage costs one line.</para></summary>
    /// <param name="ct">Cancels the in-flight stats request.</param>
    /// <returns>A <see cref="Task"/> that completes once the snapshot is refreshed.</returns>
    public async Task RefreshStatsAsync(CancellationToken ct = default)
    {
        if (Volatile.Read(ref _disposed) != 0)
        {
            return;
        }

        // No ConfigureAwait(false): Stats is bound and PerfDataChanged is contractually
        // raised on the UI thread, so the continuation must resume on the caller's
        // dispatcher (callers invoke this from the UI thread). Resuming off it would
        // update bound state from a pool thread → RPC_E_WRONG_THREAD.
        EngineStatsData? stats;
        try
        {
            using var linked = CancellationTokenSource.CreateLinkedTokenSource(
                _lifetime.Token,
                ct);
            stats = await _engine.GetStatsAsync(linked.Token);
        }
        catch (OperationCanceledException)
        {
            // The panel closed, the view model died, or the caller cancelled:
            // an abandoned poll, not a failure.
            return;
        }
        catch (ObjectDisposedException) when (Volatile.Read(ref _disposed) != 0)
        {
            // Dispose won the race against this poll and disposed _lifetime
            // between the guard above and the link — reading its Token throws.
            // The panel is gone; there is nobody left to tell.
            return;
        }
        catch (Exception ex)
        {
            // Everything else (engine unavailable, transport fault, protocol
            // error) is real but non-fatal: log it once per failure kind.
            NoteStatsFailure(ex);
            return;
        }

        NoteStatsSuccess();
        Stats = stats;
        if (Stats is { } s)
        {
            _recentWsBytes.Add(s.CurrentWsBytes);
            if (_recentWsBytes.Count > MaxRecent)
            {
                _recentWsBytes.RemoveAt(0);
            }
        }

        PerfDataChanged?.Invoke();
    }

    /// <summary>Log a contained stats failure — the first of each consecutive
    /// run of one kind only, so a persistently unreachable engine leaves one
    /// line per outage instead of one per second.</summary>
    /// <param name="ex">The failure that ended this poll.</param>
    private void NoteStatsFailure(Exception ex)
    {
        _statsFailures++;
        var kind = ex.GetType().FullName ?? ex.GetType().Name;
        if (string.Equals(kind, _statsFailureKind, StringComparison.Ordinal))
        {
            return; // same failure still repeating — already reported
        }

        _statsFailureKind = kind;
        FileLog.WarnEvent(
            "perf",
            "engine stats refresh failed",
            ex,
            ("polls", _statsFailures));
    }

    /// <summary>Close a logged outage: the counterpart line that says the panel
    /// is reading stats again, so a lone warning is never mistaken for a
    /// still-broken engine.</summary>
    private void NoteStatsSuccess()
    {
        if (_statsFailureKind is null)
        {
            return;
        }

        FileLog.Event(
            "perf",
            "engine stats refresh recovered",
            ("polls", _statsFailures));
        _statsFailureKind = null;
        _statsFailures = 0;
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

    /// <summary>Cancel in-flight stats polling and release view subscribers.</summary>
    public void Dispose()
    {
        if (Interlocked.Exchange(ref _disposed, 1) != 0)
        {
            return;
        }

        _lifetime.Cancel();
        _lifetime.Dispose();
        PerfDataChanged = null;
    }
}
