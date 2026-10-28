using FindMyFiles.Engine;
using FindMyFiles.Tests.TestDoubles;
using FindMyFiles.ViewModels;
using Xunit;

namespace FindMyFiles.Tests;

/// <summary> State-transition coverage for <see cref="SearchOrchestrator"/> that the thick existing suite left under-asserted: supersede/epoch orderings beyond a single pair (older-completes-first, three-in-flight), stale-retry recovery (not just the give-up), an index change that races an in-flight query, and the lazy request pull behind the debounce.
/// Deterministic — the stub engine and <see cref="ManualDispatcher"/> let each test choose exactly when and in which order queries complete.
/// </summary>
public sealed class SearchOrchestratorTransitionTests : IDisposable
{
    private readonly ManualDispatcher _dispatcher = new();
    private readonly StubEngineClient _engine = new();
    private readonly EngineEventMarshaler _events;
    private readonly ResultsPresenter _presenter;
    private readonly SearchOrchestrator _orchestrator;
    private SearchRequest _request = new(string.Empty, SearchOptions.Default);

    /// <summary>The 50ms debounce timer the orchestrator created in its ctor.</summary>
    private ManualDispatcher.ManualTimer Debounce => _dispatcher.Timers[0];

    public SearchOrchestratorTransitionTests()
    {
        SyncContext.RunContinuationsInline();
        _presenter = new ResultsPresenter(_dispatcher);
        _events = new EngineEventMarshaler(_engine, _dispatcher);
        _orchestrator = new SearchOrchestrator(
            _engine,
            _events,
            _dispatcher,
            _presenter,
            () => _request);
    }

    public void Dispose()
    {
        _orchestrator.Dispose();
        _presenter.Dispose();
        _events.Dispose();
    }

    [Fact]
    public void OlderQueryCompletingFirst_IsDiscarded_AndOnlyTheNewerPublishes()
    {
        var publications = new List<ResultsPublication>();
        _presenter.ResultsPublished += publications.Add;

        _request = new SearchRequest("a", SearchOptions.Default);
        _orchestrator.Requery(RequeryOrigin.Initial);
        _request = new SearchRequest("ab", SearchOptions.Default);
        _orchestrator.Requery(RequeryOrigin.Typing);
        Assert.Equal(2, _engine.Searches.Count);

        var older = _engine.Searches[0].CompleteWith(Rows.Many(3, "old"));
        Assert.True(older.Disposed);
        Assert.Empty(publications);
        Assert.Empty(_presenter.ResultsSource);

        var newer = _engine.Searches[1].CompleteWith(Rows.Many(5, "new"));
        Assert.False(newer.Disposed);
        Assert.Single(publications);
        Assert.Equal(5, _presenter.ResultsSource.Count);
        Assert.Equal("new_000000.txt", ((ResultRow)_presenter.ResultsSource[0]!).Name);
    }

    [Fact]
    public void ThreeInFlightQueries_PublishOnlyTheNewest_RegardlessOfCompletionOrder()
    {
        var publications = new List<ResultsPublication>();
        _presenter.ResultsPublished += publications.Add;

        _request = new SearchRequest("a", SearchOptions.Default);
        _orchestrator.Requery(RequeryOrigin.Initial);
        _request = new SearchRequest("ab", SearchOptions.Default);
        _orchestrator.Requery(RequeryOrigin.Typing);
        _request = new SearchRequest("abc", SearchOptions.Default);
        _orchestrator.Requery(RequeryOrigin.Typing);
        Assert.Equal(3, _engine.Searches.Count);

        var newest = _engine.Searches[2].CompleteWith(Rows.Many(7, "c"));
        var middle = _engine.Searches[1].CompleteWith(Rows.Many(4, "b"));
        var oldest = _engine.Searches[0].CompleteWith(Rows.Many(2, "a"));

        Assert.False(newest.Disposed);
        Assert.True(middle.Disposed);
        Assert.True(oldest.Disposed);
        Assert.Single(publications);
        Assert.Equal(7, _presenter.ResultsSource.Count);
        Assert.Equal("c_000000.txt", ((ResultRow)_presenter.ResultsSource[0]!).Name);
    }

    [Fact]
    public void StaleResult_RetrySucceeds_AndPublishesTheRetry()
    {
        var publications = new List<ResultsPublication>();
        _presenter.ResultsPublished += publications.Add;

        _request = new SearchRequest("a", SearchOptions.Default);
        _orchestrator.Requery(RequeryOrigin.Initial);

        var stale = new StubSearchResult(Rows.Many(3))
        {
            ThrowOnFetch = new StaleResultException(),
        };
        _engine.Searches[0].CompleteWith(stale);
        Assert.True(stale.Disposed);
        Assert.Equal(2, _engine.Searches.Count);
        Assert.Empty(publications);

        var good = _engine.Searches[1].CompleteWith(Rows.Many(5, "ok"));
        Assert.False(good.Disposed);
        Assert.Single(publications);
        Assert.Equal(5, _presenter.ResultsSource.Count);
        Assert.Equal("ok_000000.txt", ((ResultRow)_presenter.ResultsSource[0]!).Name);
    }

    [Fact]
    public void IndexChanged_SupersedesAnInFlightQuery_AndPublishesTheRequery()
    {
        var publications = new List<ResultsPublication>();
        _presenter.ResultsPublished += publications.Add;

        _request = new SearchRequest("a", SearchOptions.Default);
        _orchestrator.Requery(RequeryOrigin.Typing);
        Assert.Single(_engine.Searches);

        _engine.RaiseIndexChanged("C:");
        _dispatcher.DrainQueue();
        Assert.Equal(2, _engine.Searches.Count);

        var requery = _engine.Searches[1].CompleteWith(Rows.Many(4, "fresh"));
        var inflight = _engine.Searches[0].CompleteWith(Rows.Many(9, "stale"));
        Assert.True(inflight.Disposed);
        Assert.False(requery.Disposed);
        Assert.Single(publications);
        Assert.Equal(4, _presenter.ResultsSource.Count);
        Assert.Equal("fresh_000000.txt", ((ResultRow)_presenter.ResultsSource[0]!).Name);
    }

    [Fact]
    public void RapidTyping_ReArmsTheTimer_AndSearchesOnlyTheFinalTextPulledAtFireTime()
    {
        SyncContext.RunContinuationsInline();

        _orchestrator.NotifyTextChanged("h");
        _orchestrator.NotifyTextChanged("he");
        _orchestrator.NotifyTextChanged("hell");
        Assert.Empty(_engine.Searches);
        Assert.True(Debounce.IsStarted);
        Assert.Equal(3, Debounce.StartCount);

        _request = new SearchRequest("hello", SearchOptions.Default);
        Debounce.Fire();

        var search = Assert.Single(_engine.Searches);
        Assert.Equal("hello", search.Query);
    }
}
