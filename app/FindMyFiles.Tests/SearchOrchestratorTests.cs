using FindMyFiles.Engine;
using FindMyFiles.Tests.TestDoubles;
using FindMyFiles.ViewModels;
using Xunit;

namespace FindMyFiles.Tests;

public sealed class SearchOrchestratorTests
{
    private readonly ManualDispatcher _dispatcher = new();
    private readonly StubEngineClient _engine = new();
    private readonly ResultsPresenter _presenter;
    private readonly SearchOrchestrator _orchestrator;
    private SearchRequest _request = new(string.Empty, SearchOptions.Default);

    /// <summary>The 50ms debounce timer the orchestrator created in its ctor.</summary>
    private ManualDispatcher.ManualTimer Debounce => _dispatcher.Timers[0];

    public SearchOrchestratorTests()
    {
        _presenter = new ResultsPresenter(_dispatcher);
        _orchestrator = new SearchOrchestrator(
            _engine, new EngineEventMarshaler(_engine, _dispatcher), _dispatcher, _presenter,
            () => _request);
    }

    [Fact]
    public void SupersededQuery_ResultIsDisposed_AndNeverPublished()
    {
        SyncContext.RunContinuationsInline();
        var publications = new List<ResultsPublication>();
        _presenter.ResultsPublished += publications.Add;

        _request = new SearchRequest("a", SearchOptions.Default);
        _orchestrator.Requery(RequeryOrigin.Initial); // query 1, held by the stub
        _request = new SearchRequest("ab", SearchOptions.Default);
        _orchestrator.Requery(RequeryOrigin.Typing); // query 2, held by the stub
        Assert.Equal(2, _engine.Searches.Count);

        // The newer query completes first and gets published…
        var newer = _engine.Searches[1].CompleteWith(Rows.Many(5, "new"));
        Assert.Equal(5, _presenter.ResultsSource.Count);
        Assert.Single(publications);
        Assert.Equal("5 件", _presenter.CountText);

        // …then the superseded result arrives late: disposed, screen untouched.
        var older = _engine.Searches[0].CompleteWith(Rows.Many(3, "old"));
        Assert.True(older.Disposed);
        Assert.False(newer.Disposed);
        Assert.Equal(5, _presenter.ResultsSource.Count);
        Assert.Single(publications); // no second publication
        Assert.Equal("new_000000.txt", ((ResultRow)_presenter.ResultsSource[0]!).Name);
    }

    [Fact]
    public void EmptyQuery_NeverHitsTheEngine_AndPresentsEmptyIdempotently()
    {
        SyncContext.RunContinuationsInline();
        var resets = 0;
        _presenter.ResultsSource.CollectionChanged += (_, _) => resets++;

        // Startup: empty box → no engine call, and the list is already
        // empty, so not even a Reset fires (the startup flicker source).
        _orchestrator.Requery(RequeryOrigin.Initial);
        Assert.Empty(_engine.Searches);
        Assert.Equal(0, resets);

        // Idle USN ticks with the box still empty stay no-ops.
        _engine.RaiseIndexChanged("F:");
        _dispatcher.DrainQueue();
        Assert.Empty(_engine.Searches);
        Assert.Equal(0, resets);

        // A real query publishes; clearing it empties the screen once.
        _request = new SearchRequest("a", SearchOptions.Default);
        _orchestrator.Requery(RequeryOrigin.Typing);
        _engine.Searches[0].CompleteWith(Rows.Many(3));
        Assert.Equal(3, _presenter.ResultsSource.Count);

        _request = new SearchRequest(string.Empty, SearchOptions.Default);
        var resetsBeforeClear = resets;
        _orchestrator.NotifyTextChanged(string.Empty);
        Assert.Equal(0, _presenter.ResultsSource.Count);
        Assert.Equal(string.Empty, _presenter.CountText);
        Assert.Equal(resetsBeforeClear + 1, resets); // exactly one clearing Reset
        Assert.Single(_engine.Searches); // still only the "a" search
    }

    [Fact]
    public void ImeComposition_HoldsQueries_UntilTheCommit()
    {
        SyncContext.RunContinuationsInline();
        _orchestrator.NotifyCompositionStarted();

        // Per-keystroke binding updates during composition do nothing.
        _request = new SearchRequest("省", SearchOptions.Default);
        _orchestrator.NotifyTextChanged("省");
        Assert.False(Debounce.IsStarted);
        Assert.Empty(_engine.Searches);

        // The commit searches the final string through the normal debounce.
        _request = new SearchRequest("省察", SearchOptions.Default);
        _orchestrator.NotifyCompositionEnded("省察");
        Assert.True(Debounce.IsStarted);
        Debounce.Fire();
        Assert.Equal("省察", Assert.Single(_engine.Searches).Query);
    }

    [Fact]
    public void UnchangedRequery_SwapsTheHandle_WithoutRepublishingOrTextChurn()
    {
        SyncContext.RunContinuationsInline();
        var publications = new List<ResultsPublication>();
        _presenter.ResultsPublished += publications.Add;

        _request = new SearchRequest("a", SearchOptions.Default);
        _orchestrator.Requery(RequeryOrigin.Initial);
        var first = _engine.Searches[0].CompleteWith(Rows.Many(5));
        Assert.Single(publications);
        var countText = _presenter.CountText;

        // Idle USN tick: the engine re-ran the query and verified identical
        // results — the screen must not be touched.
        _engine.RaiseIndexChanged("F:");
        _dispatcher.DrainQueue();
        Assert.Equal(2, _engine.Searches.Count);
        var second = _engine.Searches[1]
            .CompleteWith(Rows.Many(5), new QueryTraceData { Unchanged = true });

        Assert.Single(publications); // no second Reset
        Assert.True(first.Disposed); // the handle still swapped forward…
        Assert.False(second.Disposed);
        Assert.Equal(countText, _presenter.CountText); // …and the ms text held still
        Assert.Equal(5, _presenter.ResultsSource.Count);
        Assert.Equal("row_000000.txt", ((ResultRow)_presenter.ResultsSource[0]!).Name);
    }

    [Fact]
    public void QuerySyntaxError_BecomesCountText_NotASearchFailure()
    {
        SyncContext.RunContinuationsInline();
        var failures = new List<Exception>();
        _orchestrator.SearchFailed += failures.Add;
        _engine.ThrowOnSearch = new QuerySyntaxException("unbalanced quote");

        _request = new SearchRequest("\"broken", SearchOptions.Default);
        _orchestrator.Requery(RequeryOrigin.Typing);

        Assert.Equal("クエリエラー: unbalanced quote", _presenter.CountText);
        Assert.Empty(failures);
    }

    [Fact]
    public void EngineFailure_RaisesSearchFailed_AndClearsCountText()
    {
        SyncContext.RunContinuationsInline();
        var failures = new List<Exception>();
        _orchestrator.SearchFailed += failures.Add;
        _engine.ThrowOnSearch = new EngineException("boom", 7);

        _request = new SearchRequest("a", SearchOptions.Default);
        _orchestrator.Requery(RequeryOrigin.Typing);

        Assert.Equal(string.Empty, _presenter.CountText);
        Assert.IsType<EngineException>(Assert.Single(failures));
    }

    [Fact]
    public void Typing_DebouncesUntilTheTimerFires()
    {
        SyncContext.RunContinuationsInline();
        _request = new SearchRequest("he", SearchOptions.Default);
        _orchestrator.NotifyTextChanged("h");
        _orchestrator.NotifyTextChanged("he");

        Assert.Empty(_engine.Searches); // nothing until the interval elapses
        Assert.True(Debounce.IsStarted);
        Assert.Equal(2, Debounce.StartCount); // re-armed on every keystroke

        Debounce.Fire();

        var search = Assert.Single(_engine.Searches);
        Assert.Equal("he", search.Query);
    }

    [Fact]
    public void ClearingTheQuery_BypassesTheDebounce_AndEmptiesWithoutTheEngine()
    {
        SyncContext.RunContinuationsInline();
        _request = new SearchRequest("h", SearchOptions.Default);
        _orchestrator.NotifyTextChanged("h"); // debounce armed
        Debounce.Fire();
        _engine.Searches[0].CompleteWith(Rows.Many(2));
        Assert.Equal(2, _presenter.ResultsSource.Count);

        _orchestrator.NotifyTextChanged("he"); // debounce re-armed…
        _request = new SearchRequest(string.Empty, SearchOptions.Default);
        _orchestrator.NotifyTextChanged(string.Empty);

        Assert.Equal(0, _presenter.ResultsSource.Count); // …cleared instantly
        Assert.False(Debounce.IsStarted); // the pending typing requery was cancelled
        Assert.Single(_engine.Searches); // and the empty query never hit the engine

        _dispatcher.FireTimers();
        Assert.Single(_engine.Searches); // it stays cancelled
    }

    [Fact]
    public void StaleResult_RetriesOnce_ThenGivesUp()
    {
        SyncContext.RunContinuationsInline();
        _request = new SearchRequest("a", SearchOptions.Default);
        _orchestrator.Requery(RequeryOrigin.Initial);
        var first = new StubSearchResult(Rows.Many(3))
        {
            ThrowOnFetch = new StaleResultException(),
        };
        _engine.Searches[0].CompleteWith(first);

        // The prefetch threw stale → result disposed, exactly one retry ran.
        Assert.True(first.Disposed);
        Assert.Equal(2, _engine.Searches.Count);

        var second = new StubSearchResult(Rows.Many(3))
        {
            ThrowOnFetch = new StaleResultException(),
        };
        _engine.Searches[1].CompleteWith(second);

        Assert.True(second.Disposed);
        Assert.Equal(2, _engine.Searches.Count); // stale twice → no requery storm
    }

    [Fact]
    public void FocusedSearch_RewritesTheQuery_OnlyWhileTheToggleIsOn()
    {
        SyncContext.RunContinuationsInline();
        _orchestrator.FocusedExcludePaths = [@"\windows\"];
        _orchestrator.FocusedExtensions = ["pdf"];
        _request = new SearchRequest("report", SearchOptions.Default);

        // Default off: existing behavior, the query passes through verbatim.
        _orchestrator.Requery(RequeryOrigin.Initial);
        Assert.Equal("report", _engine.Searches[0].Query);

        // The toggle path: a flip requeries as a filter change (top reset)
        // and the engine sees the rewritten query, not the user's text.
        _orchestrator.FocusedSearch = true;
        _orchestrator.Requery(RequeryOrigin.Filter);
        Assert.Equal(@"report !path:""\windows\"" ext:pdf", _engine.Searches[1].Query);

        // Off again: back to verbatim.
        _orchestrator.FocusedSearch = false;
        _orchestrator.Requery(RequeryOrigin.Filter);
        Assert.Equal("report", _engine.Searches[2].Query);
    }

    [Fact]
    public void IndexChanged_RequeriesViaTheDispatcher()
    {
        SyncContext.RunContinuationsInline();
        _request = new SearchRequest("a", SearchOptions.Default);
        _engine.RaiseIndexChanged("C:");

        Assert.Empty(_engine.Searches); // marshaled to the UI queue first
        _dispatcher.DrainQueue();
        Assert.Single(_engine.Searches);
    }
}
