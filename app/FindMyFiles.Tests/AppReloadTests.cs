using FindMyFiles.Engine;
using FindMyFiles.Services;
using Xunit;

namespace FindMyFiles.Tests;

/// <summary>
/// Unit tests for the in-process soft restart core (<see cref="AppReload"/>,
/// ADR-0036) — the part that the view-shell wiring in <c>App</c> cannot cover.
/// Pins the load-bearing ordering: the diagnostics window closes first, the
/// freshly resolved engine is published <em>before</em> the page rebuild, and the
/// old engine is disposed exactly once and only <em>after</em> the rebuild. These
/// are the invariants that, if reordered, reintroduce the disappearing-app /
/// half-torn-down-engine failure modes the relaunch design had.
/// </summary>
public sealed class AppReloadTests
{
    /// <summary>Records when it is disposed so the ordering and once-only contract
    /// can be asserted; every other member is an unused default (never called by
    /// <see cref="AppReload"/>).</summary>
    private sealed class RecordingEngine(List<string> log, string name) : IEngineClient
    {
        public int Disposes { get; private set; }

#pragma warning disable CS0067 // events are part of the interface but unused here
        public event Action<string>? IndexChanged;

        public event Action<VolumeStatus>? VolumeUpdated;

        public event Action<int>? EngineErrorOccurred;

        public event Action<EngineConnectionState>? ConnectionChanged;
#pragma warning restore CS0067

        public EngineConnectionState Connection => EngineConnectionState.InProc;

        public Task<IReadOnlyList<string>> ListVolumesAsync(CancellationToken ct = default) =>
            Task.FromResult<IReadOnlyList<string>>([]);

        public Task StartIndexingAsync(IReadOnlyList<string> volumes, CancellationToken ct = default) =>
            Task.CompletedTask;

        public Task<IReadOnlyList<VolumeStatus>> GetStatusAsync(CancellationToken ct = default) =>
            Task.FromResult<IReadOnlyList<VolumeStatus>>([]);

        public Task<SearchOutcome> SearchAsync(
            string query, SearchOptions options, CancellationToken ct = default) =>
            throw new NotSupportedException();

        public Task<EngineStatsData?> GetStatsAsync(CancellationToken ct = default) =>
            Task.FromResult<EngineStatsData?>(null);

        public void Dispose()
        {
            Disposes++;
            log.Add($"dispose:{name}");
        }
    }

    [Fact]
    public void Run_closes_diagnostics_then_swaps_the_engine_then_rebuilds_then_disposes_old()
    {
        var log = new List<string>();
        var old = new RecordingEngine(log, "old");
        var fresh = new RecordingEngine(log, "fresh");
        IEngineClient current = old;

        var reload = new AppReload(
            resolve: _ =>
            {
                log.Add("resolve");
                return fresh;
            },
            getEngine: () => current,
            setEngine: e =>
            {
                log.Add("set");
                current = e;
            },
            renavigate: () => log.Add("renavigate"),
            closeDiagnostics: () => log.Add("closeDiag"));

        reload.Run(["--engine=pipe"]);

        // The whole point of the ordering: diagnostics gone first (it polls the old
        // engine), the new engine published before the page rebuilds against it, and
        // the old engine torn down only after the rebuild.
        Assert.Equal(["closeDiag", "resolve", "set", "renavigate", "dispose:old"], log);
        Assert.Same(fresh, current);
        Assert.Equal(1, old.Disposes);
        Assert.Equal(0, fresh.Disposes); // the live engine is never disposed
    }

    [Fact]
    public void Run_ignores_a_reentrant_call_triggered_during_the_rebuild()
    {
        var log = new List<string>();
        var old = new RecordingEngine(log, "old");
        var fresh = new RecordingEngine(log, "fresh");
        IEngineClient current = old;
        AppReload reload = null!;

        reload = new AppReload(
            resolve: _ => fresh,
            getEngine: () => current,
            setEngine: e => current = e,
            renavigate: () =>
            {
                // A page rebuild that re-enters Run (e.g. a Loaded handler that
                // triggers another soft restart) must be a no-op, not a recursive
                // teardown.
                log.Add("renavigate");
                reload.Run(["--engine=pipe"]);
            },
            closeDiagnostics: () => log.Add("closeDiag"));

        reload.Run(["--engine=pipe"]);

        // Exactly one cycle ran: the nested Run (during renavigate) was rejected by
        // the re-entry guard, so closeDiag/renavigate happen once and the old engine
        // is disposed once — no recursive teardown.
        Assert.Equal(["closeDiag", "renavigate", "dispose:old"], log);
        Assert.Equal(1, old.Disposes);
    }
}
