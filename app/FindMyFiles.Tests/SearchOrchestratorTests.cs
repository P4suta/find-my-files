using System.Reflection;
using FindMyFiles.Engine;
using FindMyFiles.Highlighting;
using FindMyFiles.Tests.TestDoubles;
using FindMyFiles.ViewModels;
using Xunit;

namespace FindMyFiles.Tests;

public sealed class SearchOrchestratorTests : IDisposable
{
    private readonly ManualDispatcher _dispatcher = new();
    private readonly StubEngineClient _engine = new();
    private readonly ResultsPresenter _presenter;
    private readonly SearchOrchestrator _orchestrator;
    private readonly EngineEventMarshaler _events;
    private SearchRequest _request = new(string.Empty, SearchOptions.Default);

    /// <summary>The 50ms debounce timer the orchestrator created in its ctor.</summary>
    private ManualDispatcher.ManualTimer Debounce => _dispatcher.Timers[0];

    public SearchOrchestratorTests()
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

    /// <summary>Regression ("internal error (query.Typing)"): the engine completes SearchAsync off the UI thread (FFI Task.Run, pipe read loop), so RunQueryAsync's continuation — TraceCaptured (bound Perf state) and the presenter's Reassign/CountText (EnsureUiThread) — must resume on the captured UI SynchronizationContext.
    /// ConfigureAwait(false) ran it on the completing thread → COMException 0x8001010E (RPC_E_WRONG_THREAD).
    /// This pins the fix.</summary>
    [Fact]
    public void Query_continuation_resumes_on_the_captured_synchronization_context()
    {
        var traceFired = false;
        _orchestrator.TraceCaptured += _ => traceFired = true;

        var ui = new RecordingSyncContext();
        var saved = SynchronizationContext.Current;
        SynchronizationContext.SetSynchronizationContext(ui);
        try
        {
            _request = new SearchRequest("a", SearchOptions.Default);
            _orchestrator.Requery(RequeryOrigin.Typing);
            Assert.Single(_engine.Searches);
            Assert.False(traceFired);

            SynchronizationContext.SetSynchronizationContext(null);
            _engine.Searches[0].CompleteWith(Rows.Many(3, "r"));
            SynchronizationContext.SetSynchronizationContext(ui);

            Assert.True(ui.Posted > 0, "query continuation must marshal to the UI context");
            Assert.False(traceFired);

            ui.Drain();
            Assert.True(traceFired);
        }
        finally
        {
            SynchronizationContext.SetSynchronizationContext(saved);
        }
    }

    [Fact]
    public void SupersededQuery_ResultIsDisposed_AndNeverPublished()
    {
        SyncContext.RunContinuationsInline();
        var publications = new List<ResultsPublication>();
        var traces = new List<QueryTraceData?>();
        _presenter.ResultsPublished += publications.Add;
        _orchestrator.TraceCaptured += traces.Add;

        _request = new SearchRequest("a", SearchOptions.Default);
        _orchestrator.Requery(RequeryOrigin.Initial);
        _request = new SearchRequest("ab", SearchOptions.Default);
        _orchestrator.Requery(RequeryOrigin.Typing);
        Assert.Equal(2, _engine.Searches.Count);

        var newestTrace = new QueryTraceData { TotalUs = 20 };
        var newer = _engine.Searches[1].CompleteWith(Rows.Many(5, "new"), newestTrace);
        Assert.Equal(5, _presenter.ResultsSource.Count);
        Assert.Single(publications);
        var countText = _presenter.CountText;
        Assert.EndsWith("5 items", countText, StringComparison.Ordinal);

        var older = _engine.Searches[0].CompleteWith(
            Rows.Many(3, "old"),
            new QueryTraceData { TotalUs = 10 });
        Assert.True(older.Disposed);
        Assert.False(newer.Disposed);
        Assert.Equal(5, _presenter.ResultsSource.Count);
        Assert.Single(publications);
        Assert.Equal([newestTrace], traces);
        Assert.Equal(countText, _presenter.CountText);
        Assert.Equal("new_000000.txt", ((ResultRow)_presenter.ResultsSource[0]!).Name);
    }

    [Fact]
    public void Requery_CancelsTheSupersededEngineRequest()
    {
        SyncContext.RunContinuationsInline();
        _request = new SearchRequest("a", SearchOptions.Default);
        _orchestrator.Requery(RequeryOrigin.Initial);
        var older = Assert.Single(_engine.Searches);
        Assert.False(older.CancellationToken.IsCancellationRequested);

        _request = new SearchRequest("ab", SearchOptions.Default);
        _orchestrator.Requery(RequeryOrigin.Typing);

        Assert.True(older.CancellationToken.IsCancellationRequested);
        Assert.False(_engine.Searches[1].CancellationToken.IsCancellationRequested);
    }

    [Fact]
    public void Requery_CancelsAnOlderPagePrefetch_AndDisposesItsResult()
    {
        SyncContext.RunContinuationsInline();
        _request = new SearchRequest("a", SearchOptions.Default);
        _orchestrator.Requery(RequeryOrigin.Initial);

        var pageGate = new TaskCompletionSource(
            TaskCreationOptions.RunContinuationsAsynchronously);
        var olderResult = new StubSearchResult(Rows.Many(80, "old"))
        {
            Gate = pageGate,
            HonorCancellation = true,
        };
        _engine.Searches[0].CompleteWith(olderResult);
        Assert.Equal(1, olderResult.FetchCount);
        Assert.False(olderResult.LastFetchToken.IsCancellationRequested);

        _request = new SearchRequest("ab", SearchOptions.Default);
        _orchestrator.Requery(RequeryOrigin.Typing);
        Assert.True(olderResult.LastFetchToken.IsCancellationRequested);

        pageGate.SetResult();
        Assert.True(
            SpinWait.SpinUntil(() => olderResult.Disposed, TimeSpan.FromSeconds(2)),
            "cancelled prefetch must dispose its unpublished result");
        Assert.Empty(_presenter.ResultsSource);
    }

    [Fact]
    public void Dispose_CancelsTheActiveQuery_AndLateCompletionIsSafe()
    {
        SyncContext.RunContinuationsInline();
        _request = new SearchRequest("a", SearchOptions.Default);
        _orchestrator.Requery(RequeryOrigin.Initial);
        var pending = Assert.Single(_engine.Searches);

        _orchestrator.Dispose();
        Assert.True(pending.CancellationToken.IsCancellationRequested);

        var late = pending.CompleteWith(Rows.Many(3, "late"));
        Assert.True(late.Disposed);
        _orchestrator.Requery(RequeryOrigin.Typing);
        Assert.Single(_engine.Searches);
    }

    [Fact]
    public void EmptyQuery_NeverHitsTheEngine_AndPresentsEmptyIdempotently()
    {
        SyncContext.RunContinuationsInline();
        var resets = 0;
        var failures = new List<Exception>();
        _presenter.ResultsSource.CollectionChanged += (_, _) => resets++;
        _orchestrator.SearchFailed += failures.Add;
        _engine.ThrowOnSearch = new InvalidOperationException("empty query reached the engine");

        _orchestrator.Requery(RequeryOrigin.Initial);
        Assert.Empty(_engine.Searches);
        Assert.Equal(0, resets);

        _engine.RaiseIndexChanged("F:");
        _dispatcher.DrainQueue();
        Assert.Empty(_engine.Searches);
        Assert.Equal(0, resets);

        _engine.ThrowOnSearch = null;
        _request = new SearchRequest("a", SearchOptions.Default);
        _orchestrator.Requery(RequeryOrigin.Typing);
        _engine.Searches[0].CompleteWith(Rows.Many(3));
        Assert.Equal(3, _presenter.ResultsSource.Count);

        _request = new SearchRequest(string.Empty, SearchOptions.Default);
        _engine.ThrowOnSearch = new InvalidOperationException("clear reached the engine");
        var resetsBeforeClear = resets;
        _orchestrator.NotifyTextChanged(string.Empty);
        Assert.Empty(_presenter.ResultsSource);
        Assert.Equal(string.Empty, _presenter.CountText);
        Assert.Equal(resetsBeforeClear + 1, resets);
        Assert.Single(_engine.Searches);
        Assert.Empty(failures);
    }

    [Fact]
    public void EmptyQuery_clears_the_last_trace_without_starting_engine_work()
    {
        SyncContext.RunContinuationsInline();
        var traces = new List<QueryTraceData?>();
        _orchestrator.TraceCaptured += traces.Add;

        _orchestrator.Requery(RequeryOrigin.Initial);

        Assert.Equal([null], traces);
        Assert.Empty(_engine.Searches);
    }

    [Theory]
    [InlineData(null, true)]
    [InlineData("", true)]
    [InlineData("x", false)]
    public void TextAndGenerationClassifiers_PinBothSidesOfTheirBoundaries(
        string? value,
        bool empty)
    {
        Assert.Equal(empty, SearchOrchestrator.IsEmptyText(value));
        Assert.True(SearchOrchestrator.IsCurrentGeneration(7, 7));
        Assert.False(SearchOrchestrator.IsCurrentGeneration(7, 8));
    }

    [Fact]
    public void NotificationsAfterDispose_AreNoOps()
    {
        _orchestrator.Dispose();
        var starts = Debounce.StartCount;
        var stops = Debounce.StopCount;

        _orchestrator.NotifyTextChanged("x");
        _orchestrator.NotifyCompositionStarted();
        _orchestrator.NotifyCompositionEnded("x");

        Assert.Equal(starts, Debounce.StartCount);
        Assert.Equal(stops, Debounce.StopCount);
        Assert.Empty(_engine.Searches);
    }

    [Fact]
    public void ImeComposition_HoldsQueries_UntilTheCommit()
    {
        SyncContext.RunContinuationsInline();
        _orchestrator.NotifyCompositionStarted();

        _request = new SearchRequest("省", SearchOptions.Default);
        _orchestrator.NotifyTextChanged("省");
        Assert.False(Debounce.IsStarted);
        Assert.Empty(_engine.Searches);

        _request = new SearchRequest("省察", SearchOptions.Default);
        _orchestrator.NotifyCompositionEnded("省察");
        Assert.True(Debounce.IsStarted);
        Debounce.Fire();
        Assert.Equal("省察", Assert.Single(_engine.Searches).Query);
    }

    [Fact]
    public void ImeComposition_start_disarms_an_already_pending_debounce()
    {
        _orchestrator.NotifyTextChanged("draft");
        Assert.True(Debounce.IsStarted);
        var stops = Debounce.StopCount;

        _orchestrator.NotifyCompositionStarted();

        Assert.False(Debounce.IsStarted);
        Assert.Equal(stops + 1, Debounce.StopCount);
        Debounce.Fire();
        Assert.Empty(_engine.Searches);
    }

    [Fact]
    public void Completed_query_clears_and_disposes_its_active_operation()
    {
        SyncContext.RunContinuationsInline();
        _request = new SearchRequest("done", SearchOptions.Default);
        _orchestrator.Requery(RequeryOrigin.Initial);
        var operation = Assert.IsAssignableFrom<CancellationTokenSource>(
            Field("_activeQuery").GetValue(_orchestrator));

        Assert.Single(_engine.Searches).CompleteWith([]);

        Assert.Null(Field("_activeQuery").GetValue(_orchestrator));
        Assert.Throws<ObjectDisposedException>(() => _ = operation.Token);
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

        _engine.RaiseIndexChanged("F:");
        _dispatcher.DrainQueue();
        Assert.Equal(2, _engine.Searches.Count);
        var second = _engine.Searches[1]
            .CompleteWith(Rows.Many(5), new QueryTraceData { Unchanged = true });

        Assert.Single(publications);
        Assert.True(first.Disposed);
        Assert.False(second.Disposed);
        Assert.Equal(countText, _presenter.CountText);
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

        Assert.Equal("Query error: unbalanced quote", _presenter.CountText);
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
    public void EngineUnavailable_RaisesSearchFailed_AndClearsCountText()
    {
        SyncContext.RunContinuationsInline();
        var failures = new List<Exception>();
        _orchestrator.SearchFailed += failures.Add;
        _engine.ThrowOnSearch = new EngineUnavailableException("service disconnected");

        _request = new SearchRequest("a", SearchOptions.Default);
        _orchestrator.Requery(RequeryOrigin.Typing);

        Assert.Equal(string.Empty, _presenter.CountText);
        Assert.IsType<EngineUnavailableException>(Assert.Single(failures));
    }

    [Fact]
    public void Typing_DebouncesUntilTheTimerFires()
    {
        SyncContext.RunContinuationsInline();
        _request = new SearchRequest("he", SearchOptions.Default);
        _orchestrator.NotifyTextChanged("h");
        _orchestrator.NotifyTextChanged("he");

        Assert.Empty(_engine.Searches);
        Assert.True(Debounce.IsStarted);
        Assert.Equal(2, Debounce.StartCount);

        Debounce.Fire();

        var search = Assert.Single(_engine.Searches);
        Assert.Equal("he", search.Query);
    }

    [Fact]
    public void ClearingTheQuery_BypassesTheDebounce_AndEmptiesWithoutTheEngine()
    {
        SyncContext.RunContinuationsInline();
        _request = new SearchRequest("h", SearchOptions.Default);
        _orchestrator.NotifyTextChanged("h");
        Debounce.Fire();
        _engine.Searches[0].CompleteWith(Rows.Many(2));
        Assert.Equal(2, _presenter.ResultsSource.Count);

        _orchestrator.NotifyTextChanged("he");
        _request = new SearchRequest(string.Empty, SearchOptions.Default);
        _orchestrator.NotifyTextChanged(string.Empty);

        Assert.Empty(_presenter.ResultsSource);
        Assert.False(Debounce.IsStarted);
        Assert.Single(_engine.Searches);

        _dispatcher.FireTimers();
        Assert.Single(_engine.Searches);
    }

    [Fact]
    public void StaleResult_RetriesOnce_ThenGivesUp()
    {
        SyncContext.RunContinuationsInline();
        using var log = new LogCapture();
        _request = new SearchRequest("a", SearchOptions.Default);
        _orchestrator.Requery(RequeryOrigin.Initial);
        var first = new StubSearchResult(Rows.Many(3))
        {
            ThrowOnFetch = new StaleResultException(),
        };
        _engine.Searches[0].CompleteWith(first);

        Assert.True(first.Disposed);
        Assert.Equal(2, _engine.Searches.Count);

        var second = new StubSearchResult(Rows.Many(3))
        {
            ThrowOnFetch = new StaleResultException(),
        };
        _engine.Searches[1].CompleteWith(second);

        Assert.True(second.Disposed);
        Assert.Equal(2, _engine.Searches.Count);
        AssertLog(log.Text.Split('\n'), "area=query", "qlen=1", "result stale twice in a row");
    }

    [Fact]
    public void FocusedSearch_RewritesTheQuery_OnlyWhileTheToggleIsOn()
    {
        SyncContext.RunContinuationsInline();
        _orchestrator.FocusedExcludePathsForTests = [@"\windows\"];
        _orchestrator.FocusedExtensionsForTests = ["pdf"];
        _request = new SearchRequest("report", SearchOptions.Default);

        _orchestrator.Requery(RequeryOrigin.Initial);
        Assert.Equal("report", _engine.Searches[0].Query);

        _orchestrator.FocusedSearch = true;
        _orchestrator.Requery(RequeryOrigin.Filter);
        Assert.Equal(@"report !path:""\windows\"" ext:pdf", _engine.Searches[1].Query);

        _orchestrator.FocusedSearch = false;
        _orchestrator.Requery(RequeryOrigin.Filter);
        Assert.Equal("report", _engine.Searches[2].Query);
    }

    [Fact]
    public void RegexMode_PassesOptionsThrough_AndSuppressesFocusedRewrite()
    {
        SyncContext.RunContinuationsInline();
        _orchestrator.FocusedExcludePathsForTests = [@"\windows\"];
        _orchestrator.FocusedExtensionsForTests = ["pdf"];
        _orchestrator.FocusedSearch = true;

        var opts = new SearchOptions(
            FmfSort.Name,
            false,
            FmfCase.Smart,
            IncludeHiddenSystem: false,
            RegexMode: true,
            Scope: RegexScope.Path);
        _request = new SearchRequest(@"report.*\.pdf$", opts);
        _orchestrator.Requery(RequeryOrigin.Filter);

        var search = Assert.Single(_engine.Searches);

        Assert.True(search.Options.RegexMode);
        Assert.Equal(RegexScope.Path, search.Options.Scope);

        Assert.Equal(@"report.*\.pdf$", search.Query);
    }

    [Theory]
    [InlineData(false, false)]
    [InlineData(true, true)]
    public void RegexMode_selects_the_matching_highlighter_for_published_rows(
        bool regexMode,
        bool expectedHighlight)
    {
        SyncContext.RunContinuationsInline();
        _request = new SearchRequest(
            "^report",
            SearchOptions.Default with
            {
                RegexMode = regexMode,
                Scope = RegexScope.Name,
            });

        _orchestrator.Requery(RequeryOrigin.Filter);
        Assert.Single(_engine.Searches).CompleteWith([Rows.File(1, "report.txt")]);

        var row = Assert.IsType<ResultRow>(_presenter.ResultsSource[0]);
        if (expectedHighlight)
        {
            Assert.Equal([new HighlightRange(0, 6)], row.NameRanges);
        }
        else
        {
            Assert.Empty(row.NameRanges);
        }
    }

    [Fact]
    public void Failure_paths_clear_visible_status_and_emit_complete_safe_logs()
    {
        SyncContext.RunContinuationsInline();
        using var log = new LogCapture();
        var failures = new List<Exception>();
        _orchestrator.SearchFailed += failures.Add;
        _request = new SearchRequest("abc", SearchOptions.Default);

        _presenter.CountText = "visible";
        _engine.ThrowOnSearch = new EngineUnavailableException("private path");
        _orchestrator.Requery(RequeryOrigin.Typing);
        Assert.Equal(string.Empty, _presenter.CountText);

        _presenter.CountText = "visible";
        _engine.ThrowOnSearch = new EngineException("private query", 7);
        _orchestrator.Requery(RequeryOrigin.Typing);
        Assert.Equal(string.Empty, _presenter.CountText);

        _engine.ThrowOnSearch = new InvalidOperationException("private unexpected detail");
        _orchestrator.Requery(RequeryOrigin.Typing);

        Assert.Collection(
            failures,
            item => Assert.IsType<EngineUnavailableException>(item),
            item => Assert.IsType<EngineException>(item),
            item => Assert.IsType<InvalidOperationException>(item));
        var lines = log.Text.Split('\n');
        AssertLog(lines, "area=query", "qlen=3", "engine unavailable");
        AssertLog(lines, "area=query", "qlen=3", "engine error");
        AssertLog(lines, "area=query", "qlen=3", "unexpected query failure");
        Assert.DoesNotContain("private path", log.Text, StringComparison.Ordinal);
        Assert.DoesNotContain("private query", log.Text, StringComparison.Ordinal);
        Assert.DoesNotContain("private unexpected detail", log.Text, StringComparison.Ordinal);
    }

    [Fact]
    public void Constructor_and_dispose_own_exactly_one_auto_requery_subscription()
    {
        Assert.Equal(1, SubscriberCount(_presenter.ResultsSource, "BecameStale"));
        Assert.Equal(1, SubscriberCount(_events, "IndexChanged"));

        _orchestrator.Dispose();

        Assert.Equal(0, SubscriberCount(_presenter.ResultsSource, "BecameStale"));
        Assert.Equal(0, SubscriberCount(_events, "IndexChanged"));
    }

    [Fact]
    public void Dispose_stops_a_pending_debounce()
    {
        var lifetime = Assert.IsType<CancellationTokenSource>(
            Field("_lifetime").GetValue(_orchestrator));
        _orchestrator.NotifyTextChanged("pending");
        Assert.True(Debounce.IsStarted);

        _orchestrator.Dispose();

        Assert.False(Debounce.IsStarted);
        Assert.Throws<ObjectDisposedException>(() => _ = lifetime.Token);
    }

    [Fact]
    public void IndexChanged_RequeriesViaTheDispatcher()
    {
        SyncContext.RunContinuationsInline();
        _request = new SearchRequest("a", SearchOptions.Default);
        _engine.RaiseIndexChanged("C:");

        Assert.Empty(_engine.Searches);
        _dispatcher.DrainQueue();
        Assert.Single(_engine.Searches);
    }

    private static FieldInfo Field(string name) =>
        typeof(SearchOrchestrator).GetField(name, BindingFlags.Instance | BindingFlags.NonPublic)
        ?? throw new InvalidOperationException($"missing field {name}");

    private static int SubscriberCount(object owner, string eventName) =>
        (owner.GetType().GetField(eventName, BindingFlags.Instance | BindingFlags.NonPublic)
            ?.GetValue(owner) as Delegate)?.GetInvocationList().Length ?? 0;

    private static void AssertLog(string[] lines, params string[] fragments) =>
        Assert.Contains(
            lines,
            line => fragments.All(fragment => line.Contains(fragment, StringComparison.Ordinal)));

    public void Dispose()
    {
        _orchestrator.Dispose();
        _presenter.Dispose();
        _events.Dispose();
    }
}
