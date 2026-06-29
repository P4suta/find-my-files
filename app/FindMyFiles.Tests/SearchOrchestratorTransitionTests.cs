using FindMyFiles.Engine;
using FindMyFiles.Tests.TestDoubles;
using FindMyFiles.ViewModels;
using Xunit;

namespace FindMyFiles.Tests;

/// <summary>
/// State-transition coverage for <see cref="SearchOrchestrator"/> that the
/// thick existing suite left under-asserted: supersede/epoch orderings beyond a
/// single pair (older-completes-first, three-in-flight), stale-retry recovery
/// (not just the give-up), an index change that races an in-flight query, and
/// the lazy request pull behind the debounce. Deterministic — the stub engine
/// and <see cref="ManualDispatcher"/> let each test choose exactly when and in
/// which order queries complete.
/// </summary>
public sealed class SearchOrchestratorTransitionTests
{
    private readonly ManualDispatcher _dispatcher = new();
    private readonly StubEngineClient _engine = new();
    private readonly ResultsPresenter _presenter;
    private readonly SearchOrchestrator _orchestrator;
    private SearchRequest _request = new(string.Empty, SearchOptions.Default);

    /// <summary>The 50ms debounce timer the orchestrator created in its ctor.</summary>
    private ManualDispatcher.ManualTimer Debounce => _dispatcher.Timers[0];

    public SearchOrchestratorTransitionTests()
    {
        // Awaited stub tasks resume inline, so CompleteWith drives the
        // continuation synchronously and each transition is deterministic.
        SyncContext.RunContinuationsInline();
        _presenter = new ResultsPresenter(_dispatcher);
        _orchestrator = new SearchOrchestrator(
            _engine,
            new EngineEventMarshaler(_engine, _dispatcher),
            _dispatcher,
            _presenter,
            () => _request);
    }

    [Fact]
    public void OlderQueryCompletingFirst_IsDiscarded_AndOnlyTheNewerPublishes()
    {
        var publications = new List<ResultsPublication>();
        _presenter.ResultsPublished += publications.Add;

        _request = new SearchRequest("a", SearchOptions.Default);
        _orchestrator.Requery(RequeryOrigin.Initial); // query 1 (older), held by the stub
        _request = new SearchRequest("ab", SearchOptions.Default);
        _orchestrator.Requery(RequeryOrigin.Typing); // query 2 (newer) — bumps the generation
        Assert.Equal(2, _engine.Searches.Count);

        // The OLDER query is first to come back — it is already superseded, so
        // it is disposed without ever publishing. (The existing suite only
        // covers the older response arriving LAST; this pins the inverse order.)
        var older = _engine.Searches[0].CompleteWith(Rows.Many(3, "old"));
        Assert.True(older.Disposed);
        Assert.Empty(publications);
        Assert.Empty(_presenter.ResultsSource);

        // Then the newest result publishes exactly once.
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

        // Newest first, then the two stale epochs out of order — only the
        // newest is ever published; both older epochs are disposed. Generation
        // correctness must hold across more than a single pair.
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

        // First prefetch goes stale → the result is disposed and exactly one
        // retry is issued (origin==Stale).
        var stale = new StubSearchResult(Rows.Many(3))
        {
            ThrowOnFetch = new StaleResultException(),
        };
        _engine.Searches[0].CompleteWith(stale);
        Assert.True(stale.Disposed);
        Assert.Equal(2, _engine.Searches.Count);
        Assert.Empty(publications);

        // The retry returns clean rows — it publishes, recovering the screen.
        // (The existing suite only covers stale-twice giving up; this pins the
        // recovery path where the single retry succeeds.)
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
        _orchestrator.Requery(RequeryOrigin.Typing); // query 1 — left in flight
        Assert.Single(_engine.Searches);

        // A USN-driven index change arrives before query 1 came back: it
        // marshals to the UI queue and, once drained, issues a fresh query that
        // supersedes the in-flight one.
        _engine.RaiseIndexChanged("C:");
        _dispatcher.DrainQueue();
        Assert.Equal(2, _engine.Searches.Count);

        // The index-triggered requery publishes; the now-stale in-flight query
        // is disposed when it finally returns.
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
        // Null the sync context HERE, in the method body (not just the ctor): xunit
        // re-installs its async-tracking context right before the test runs, so a
        // ctor-only null is overwritten. This test deliberately leaves the fired
        // search uncompleted, so its production await must run under a null context
        // or the never-completed operation stalls the runner at teardown
        // (SyncContext.RunContinuationsInline docs).
        SyncContext.RunContinuationsInline();

        // Each keystroke re-arms the one-shot debounce; nothing reaches the
        // engine until it fires.
        _orchestrator.NotifyTextChanged("h");
        _orchestrator.NotifyTextChanged("he");
        _orchestrator.NotifyTextChanged("hell");
        Assert.Empty(_engine.Searches);
        Assert.True(Debounce.IsStarted);
        Assert.Equal(3, Debounce.StartCount); // re-armed on every keystroke

        // The request is pulled lazily when the timer fires, not captured per
        // keystroke: only the final committed text is searched, exactly once.
        _request = new SearchRequest("hello", SearchOptions.Default);
        Debounce.Fire();

        var search = Assert.Single(_engine.Searches);
        Assert.Equal("hello", search.Query);
    }
}
