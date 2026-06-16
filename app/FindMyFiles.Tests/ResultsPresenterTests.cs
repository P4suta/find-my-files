using FindMyFiles.Highlighting;
using FindMyFiles.Tests.TestDoubles;
using FindMyFiles.ViewModels;
using Xunit;

namespace FindMyFiles.Tests;

/// <summary>Behavioural tests for <see cref="ResultsPresenter"/> — the result
/// lifecycle (seeded publish, in-place refresh, empty, stale disposal) that the
/// flicker-free virtualization depends on. Driven by <see cref="StubSearchResult"/>
/// + <see cref="ManualDispatcher"/>; no real engine or list view.</summary>
public sealed class ResultsPresenterTests
{
    private readonly ManualDispatcher _dispatcher = new();
    private readonly ResultsPresenter _presenter;

    public ResultsPresenterTests() => _presenter = new ResultsPresenter(_dispatcher);

    [Fact]
    public async Task PublishAsync_seeds_publishes_and_announces()
    {
        var pubs = new List<ResultsPublication>();
        _presenter.ResultsPublished += pubs.Add;
        var result = new StubSearchResult(Rows.Many(5));

        await _presenter.PublishAsync(
            result, null, RequeryOrigin.Initial, CompiledHighlighter.Empty, () => true);

        Assert.Equal(5, _presenter.ResultsSource.Count);
        Assert.Equal("5 items", _presenter.CountText);
        Assert.Single(pubs);
        Assert.Equal(RequeryOrigin.Initial, pubs[0].Origin);
        Assert.False(result.Disposed);
    }

    [Fact]
    public async Task PublishAsync_superseded_mid_flight_disposes_without_publishing()
    {
        var pubs = new List<ResultsPublication>();
        _presenter.ResultsPublished += pubs.Add;
        var result = new StubSearchResult(Rows.Many(5));

        await _presenter.PublishAsync(
            result, null, RequeryOrigin.Typing, CompiledHighlighter.Empty, () => false);

        Assert.True(result.Disposed);
        Assert.Empty(pubs);
        Assert.Empty(_presenter.ResultsSource);
    }

    [Fact]
    public async Task PublishAsync_prefetch_failure_disposes_and_rethrows()
    {
        var result = new StubSearchResult(Rows.Many(5))
        {
            ThrowOnFetch = new InvalidOperationException("boom"),
        };

        await Assert.ThrowsAsync<InvalidOperationException>(() =>
            _presenter.PublishAsync(
                result, null, RequeryOrigin.Initial, CompiledHighlighter.Empty, () => true));

        Assert.True(result.Disposed);
    }

    [Fact]
    public async Task PresentEmpty_after_a_publish_clears_results_and_count()
    {
        await _presenter.PublishAsync(
            new StubSearchResult(Rows.Many(3)),
            null,
            RequeryOrigin.Initial,
            CompiledHighlighter.Empty,
            () => true);
        Assert.Equal(3, _presenter.ResultsSource.Count);

        _presenter.PresentEmpty();

        Assert.Empty(_presenter.ResultsSource);
        Assert.Equal(string.Empty, _presenter.CountText);
    }

    [Fact]
    public async Task RefreshInPlaceAsync_falls_back_to_publish_when_the_count_changed()
    {
        await _presenter.PublishAsync(
            new StubSearchResult(Rows.Many(5)),
            null,
            RequeryOrigin.Initial,
            CompiledHighlighter.Empty,
            () => true);
        var pubs = new List<ResultsPublication>();
        _presenter.ResultsPublished += pubs.Add;

        await _presenter.RefreshInPlaceAsync(
            new StubSearchResult(Rows.Many(8)),
            null,
            RequeryOrigin.IndexChanged,
            CompiledHighlighter.Empty,
            () => true);

        Assert.Equal(8, _presenter.ResultsSource.Count);
        Assert.Single(pubs); // the fallback went through the announcing publish path
    }

    [Fact]
    public async Task RefreshInPlaceAsync_same_count_refreshes_without_announcing()
    {
        await _presenter.PublishAsync(
            new StubSearchResult(Rows.Many(5)),
            null,
            RequeryOrigin.Initial,
            CompiledHighlighter.Empty,
            () => true);
        var pubs = new List<ResultsPublication>();
        _presenter.ResultsPublished += pubs.Add;

        await _presenter.RefreshInPlaceAsync(
            new StubSearchResult(Rows.Many(5)),
            null,
            RequeryOrigin.IndexChanged,
            CompiledHighlighter.Empty,
            () => true);

        Assert.Empty(pubs); // RefreshInPlace path raises no Reset/announce
        Assert.Equal(5, _presenter.ResultsSource.Count);
    }

    [Fact]
    public void PresentQueryError_sets_the_count_text()
    {
        _presenter.PresentQueryError("bad query");

        Assert.Equal(StatusFormatter.QueryError("bad query"), _presenter.CountText);
    }
}
