using FindMyFiles.Engine;
using FindMyFiles.Tests.TestDoubles;
using FindMyFiles.ViewModels;
using Xunit;

namespace FindMyFiles.Tests;

/// <summary>Behavioural tests for <see cref="PerfPanelViewModel"/> — the F12
/// panel's state (trace history with a FIFO cap, stats pull, toggle), built on
/// the existing engine stub.</summary>
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
        Assert.Single(_vm.RecentTotalsUs); // the null trace contributes no total
    }

    [Fact]
    public void RecentTotals_keep_only_the_most_recent_64()
    {
        for (var i = 0; i < 70; i++)
        {
            _vm.RecordTrace(new QueryTraceData { TotalUs = (ulong)i });
        }

        Assert.Equal(64, _vm.RecentTotalsUs.Count);
        Assert.Equal(6UL, _vm.RecentTotalsUs[0]);    // 0..5 dropped off the front
        Assert.Equal(69UL, _vm.RecentTotalsUs[^1]);
    }

    [Fact]
    public async Task RefreshStatsAsync_pulls_from_the_engine_and_raises()
    {
        var raised = 0;
        _vm.PerfDataChanged += () => raised++;

        await _vm.RefreshStatsAsync();

        // The stub returns null stats; the behaviour under test is the pull +
        // assignment + notification, not a specific snapshot value.
        Assert.Null(_vm.Stats);
        Assert.Equal(1, raised);
    }
}
