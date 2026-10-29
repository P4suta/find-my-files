using FindMyFiles.Engine;
using FindMyFiles.Tests.TestDoubles;
using FindMyFiles.ViewModels;
using Xunit;

namespace FindMyFiles.Tests;

/// <summary>Behavioural tests for <see cref="PerfPanelViewModel"/> — the F12 panel's state (trace history with a FIFO cap, stats pull, toggle), built on the existing engine stub.</summary>
public sealed class PerfPanelViewModelTests
{
    private readonly StubEngineClient _engine = new();
    private readonly PerfPanelViewModel _vm;

    public PerfPanelViewModelTests() => _vm = new PerfPanelViewModel(_engine);

    [Fact]
    public void Toggle_flips_is_open()
    {
        Assert.False(_vm.IsOpen);

        _vm.Toggle();
        Assert.True(_vm.IsOpen);

        _vm.Toggle();
        Assert.False(_vm.IsOpen);
    }

    [Fact]
    public void RecordTrace_appends_total_sets_last_trace_and_raises()
    {
        var raised = 0;
        _vm.PerfDataChanged += () => raised++;
        var trace = new QueryTraceData { TotalUs = 1234 };

        _vm.RecordTrace(trace);

        Assert.Same(trace, _vm.LastTrace);
        Assert.Equal(1234UL, Assert.Single(_vm.RecentTotalsUs));
        Assert.Equal(1, raised);
    }

    [Fact]
    public void RecordTrace_null_clears_last_trace_without_extending_history()
    {
        _vm.RecordTrace(new QueryTraceData { TotalUs = 5 });

        _vm.RecordTrace(null);

        Assert.Null(_vm.LastTrace);
        Assert.Single(_vm.RecentTotalsUs);
    }

    [Fact]
    public void RecentTotals_keep_only_the_most_recent_64()
    {
        for (var i = 0; i < 70; i++)
        {
            _vm.RecordTrace(new QueryTraceData { TotalUs = (ulong)i });
        }

        Assert.Equal(64, _vm.RecentTotalsUs.Count);
        Assert.Equal(6UL, _vm.RecentTotalsUs[0]);
        Assert.Equal(69UL, _vm.RecentTotalsUs[^1]);
    }

    [Fact]
    public void Stats_publishes_the_recent_query_feed_and_notifies_when_it_swaps()
    {
        var notified = new List<string?>();
        _vm.PropertyChanged += (_, e) => notified.Add(e.PropertyName);

        _vm.Stats = new EngineStatsData
        {
            RecentQueries = Enumerable.Range(0, 12)
                .Select(i => new QueryTraceData { TotalUs = (ulong)i, Driver = "suffix" })
                .ToList(),
        };

        Assert.Contains(nameof(PerfPanelViewModel.RecentQueriesTail), notified, StringComparer.Ordinal);

        Assert.Equal(8, _vm.RecentQueriesTail.Count);
        Assert.Equal(4UL, _vm.RecentQueriesTail[0].TotalUs);
        Assert.Equal(11UL, _vm.RecentQueriesTail[^1].TotalUs);
    }

    [Fact]
    public void RecentQueriesTail_is_empty_before_the_first_snapshot()
    {
        Assert.Null(_vm.Stats);
        Assert.Empty(_vm.RecentQueriesTail);
    }

    [Fact]
    public async Task RefreshStatsAsync_pulls_from_the_engine_and_raises()
    {
        var raised = 0;
        _vm.PerfDataChanged += () => raised++;

        await _vm.RefreshStatsAsync(TestContext.Current.CancellationToken);

        Assert.Null(_vm.Stats);
        Assert.Equal(1, raised);
    }

    [Fact]
    public async Task RefreshStatsAsync_after_dispose_is_a_silent_no_op()
    {
        _vm.Dispose();

        await _vm.RefreshStatsAsync(TestContext.Current.CancellationToken);

        Assert.Null(_vm.Stats);
    }

    [Fact]
    public async Task RefreshStatsAsync_contains_engine_failures()
    {
        var raised = 0;
        _vm.PerfDataChanged += () => raised++;
        _engine.ThrowOnStats = new InvalidOperationException("engine down");

        await _vm.RefreshStatsAsync(TestContext.Current.CancellationToken);

        Assert.Null(_vm.Stats);
        Assert.Equal(0, raised);
    }

    [Fact]
    public async Task RefreshStatsAsync_contains_a_cancellation_it_did_not_request()
    {
        _engine.ThrowOnStats = new OperationCanceledException();

        await _vm.RefreshStatsAsync(TestContext.Current.CancellationToken);

        Assert.Null(_vm.Stats);
    }
}
