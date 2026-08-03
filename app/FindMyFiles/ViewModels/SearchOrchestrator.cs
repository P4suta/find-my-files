using FindMyFiles.Engine;
using FindMyFiles.Highlighting;
using FindMyFiles.Services;

namespace FindMyFiles.ViewModels;

/// <summary>
/// When and what to search: 50ms debounce on typing (clearing is instant), a
/// per-query cancellation plus a generation counter that discards work which
/// ignores cancellation, requery triggers (index changes, stale results) and
/// exception classification. Results are
/// handed to the <see cref="ResultsPresenter"/>; failures surface through
/// <see cref="SearchFailed"/> so the ViewModel owns the user-facing wording.
/// All entry points run on the UI thread.
/// </summary>
internal sealed class SearchOrchestrator : IDisposable
{
    private readonly IEngineClient _engine;
    private readonly EngineEventMarshaler _engineEvents;
    private readonly ResultsPresenter _presenter;
    private readonly Func<SearchRequest> _request;
    private readonly IDispatcherTimer _debounce;
    private readonly CancellationTokenSource _lifetime = new();
    private readonly Action _staleHandler;
    private readonly Action<string> _indexChangedHandler;
    private CancellationTokenSource? _activeQuery;
    private long _generation;
    private int _disposed;

    /// <summary>Focused search: when on, the user's query is rewritten with
    /// the two lists below (<see cref="FocusedQueryRewriter"/>) right before
    /// it reaches the engine. Defaults to off here — product wiring
    /// (MainViewModel) pushes the persisted settings in; a toggle flip is a
    /// filter change, so the owner requeries with
    /// <see cref="RequeryOrigin.Filter"/> (top reset).</summary>
    public bool FocusedSearch { get; set; }

#if FMF_TEST_SEAMS
    /// <summary>Unit-test override for the code-owned focused policy.</summary>
    internal IReadOnlyList<string>? FocusedExcludePathsForTests { get; set; }

    /// <summary>Unit-test override for the code-owned focused policy.</summary>
    internal IReadOnlyList<string>? FocusedExtensionsForTests { get; set; }
#endif

    /// <summary>Stage trace of the last completed query (null when the
    /// engine produced none) — perf-panel food.</summary>
    public event Action<QueryTraceData?>? TraceCaptured;

    /// <summary>Engine or unexpected failure (never query syntax — that goes
    /// to the presenter as count text).</summary>
    public event Action<Exception>? SearchFailed;

    /// <summary>Wires the orchestrator to its collaborators and subscribes the
    /// auto-requery triggers (stale results, index changes).</summary>
    /// <param name="engine">Engine the queries are issued against.</param>
    /// <param name="engineEvents">UI-thread-marshaled engine events; its
    /// <c>IndexChanged</c> drives an automatic requery.</param>
    /// <param name="dispatcher">UI dispatcher — used to create the debounce timer.</param>
    /// <param name="presenter">Sink that publishes results and stale signals.</param>
    /// <param name="request">Pull of the current UI state at query time (the
    /// ViewModel stays the source of truth).</param>
    public SearchOrchestrator(
        IEngineClient engine,
        EngineEventMarshaler engineEvents,
        IDispatcher dispatcher,
        ResultsPresenter presenter,
        Func<SearchRequest> request)
    {
        _engine = engine;
        _engineEvents = engineEvents;
        _presenter = presenter;
        _request = request;
        _debounce = dispatcher.CreateOneShotTimer(
            TimeSpan.FromMilliseconds(50),
            () => Requery(RequeryOrigin.Typing));

        _staleHandler = () => Requery(RequeryOrigin.Stale);
        _presenter.ResultsSource.BecameStale += _staleHandler;

        // Already on the UI thread — the marshaler is the crossing point.
        _indexChangedHandler = _ => Requery(RequeryOrigin.IndexChanged);
        engineEvents.IndexChanged += _indexChangedHandler;
    }

    private bool _composing;

    /// <summary>Search box text changed: debounce a normal edit (50ms),
    /// requery immediately on a clear (so emptying feels instant), and ignore
    /// edits while an IME composition is in flight.</summary>
    /// <param name="value">The current search box text after the edit.</param>
    public void NotifyTextChanged(string value)
    {
        if (Volatile.Read(ref _disposed) != 0)
        {
            return;
        }

        if (_composing)
        {
            return; // IME composition in flight — wait for the commit
        }

        if (IsEmptyText(value))
        {
            _debounce.Stop();
            Requery(RequeryOrigin.Clear); // clearing should feel instant
        }
        else
        {
            _debounce.Start();
        }
    }

    /// <summary>IME composition began: hold queries so half-composed text
    /// (romaji fragments, candidate strings) never hits the engine.</summary>
    public void NotifyCompositionStarted()
    {
        if (Volatile.Read(ref _disposed) != 0)
        {
            return;
        }

        _composing = true;
        _debounce.Stop();
    }

    /// <summary>IME composition committed (or cancelled) — search the final
    /// text through the normal debounce.</summary>
    /// <param name="value">The committed search box text after composition.</param>
    public void NotifyCompositionEnded(string value)
    {
        if (Volatile.Read(ref _disposed) != 0)
        {
            return;
        }

        _composing = false;
        NotifyTextChanged(value);
    }

    /// <summary>Fire-and-forget a query for the current UI state, bumping the
    /// generation so any in-flight older response is discarded.
    /// <paramref name="origin"/> records why (and lets the presenter decide
    /// whether to preserve scroll/selection).</summary>
    /// <param name="origin">Why the requery was triggered.</param>
    [System.Diagnostics.CodeAnalysis.SuppressMessage(
        "Reliability",
        "CA2000:Dispose objects before losing scope",
        Justification = "Ownership is transferred to RunQueryAsync, whose finally always disposes it.")]
    [System.Diagnostics.CodeAnalysis.SuppressMessage(
        "Reliability",
        "CA2025:Do not pass IDisposable instances into unawaited tasks",
        Justification = "The tracked fire-and-forget task owns and disposes the per-query CTS in finally.")]
    public void Requery(RequeryOrigin origin)
    {
        if (Volatile.Read(ref _disposed) != 0)
        {
            return;
        }

        CancellationTokenSource operation;
        try
        {
            operation = CancellationTokenSource.CreateLinkedTokenSource(_lifetime.Token);
        }
        catch (ObjectDisposedException) when (Volatile.Read(ref _disposed) != 0)
        {
            return;
        }

        if (Volatile.Read(ref _disposed) != 0)
        {
            operation.Dispose();
            return;
        }

        var previous = Interlocked.Exchange(ref _activeQuery, operation);
        CancelNoThrow(previous);
        RunQueryAsync(origin, operation).Forget($"query.{origin}");
    }

    private static void CancelNoThrow(CancellationTokenSource? operation)
    {
        try
        {
            operation?.Cancel();
        }
        catch (ObjectDisposedException)
        {
            // Completion won the exchange/dispose race; there is no work
            // left to cancel.
        }
    }

    private async Task RunQueryAsync(
        RequeryOrigin origin,
        CancellationTokenSource operation)
    {
        try
        {
            await RunQueryCoreAsync(origin, operation.Token);
        }
        finally
        {
            Interlocked.CompareExchange(ref _activeQuery, null, operation);
            operation.Dispose();
        }
    }

    private async Task RunQueryCoreAsync(RequeryOrigin origin, CancellationToken ct)
    {
        ct.ThrowIfCancellationRequested();
        var generation = Interlocked.Increment(ref _generation);
        var request = _request();

        // Product rule: no query, no results — a match-all listing would
        // also churn on every USN tick (its ids keep changing).
        if (string.IsNullOrWhiteSpace(request.Query))
        {
            TraceCaptured?.Invoke(null);
            _presenter.PresentEmpty();
            return;
        }

        // Focused mode is a pure rewrite at the last moment — the ViewModel
        // keeps the user's text and the engine sees the effective query. Logs
        // record only its scalar length, never its contents. It is
        // suppressed in regex mode: the engine treats the whole text as one
        // pattern, so appended !path:/ext: terms would corrupt the regex.
        var query = request.Query;
        if (FocusedSearch && !request.Options.RegexMode)
        {
#if FMF_TEST_SEAMS
            query = FocusedExcludePathsForTests is { } paths
                && FocusedExtensionsForTests is { } extensions
                    ? FocusedQueryRewriter.Compose(query, paths, extensions)
                    : FocusedQueryRewriter.Compose(query);
#else
            query = FocusedQueryRewriter.Compose(query);
#endif
        }

        var queryLength = query.EnumerateRunes().Count();

        // Highlight the user's raw words, not the focused-mode rewrite: the
        // appended !path:/ext: filters are not what the user typed. In regex
        // mode the whole query is the pattern (ADR-0023).
        IHighlighter highlighter = request.Options.RegexMode
            ? MatchHighlighter.CompileRegex(request.Query, request.Options.Scope)
            : MatchHighlighter.Compile(request.Query);
        try
        {
            // No ConfigureAwait(false): RunQueryAsync starts on the UI thread, and
            // every await below resumes into UI work — TraceCaptured (bound Perf
            // state) and the presenter's Reassign/CountText (EnsureUiThread). The
            // engine completes off the UI thread (FFI Task.Run, pipe read loop), so
            // resuming on the captured dispatcher is what keeps that work on the UI
            // thread; ConfigureAwait(false) here threw RPC_E_WRONG_THREAD under the
            // in-proc engine (.editorconfig disables CA2007/MA0004 for this reason).
            var outcome = await _engine.SearchAsync(
                query,
                request.Options,
                _presenter.PresentationBasis,
                ct);
            if (!IsCurrentGeneration(generation, Interlocked.Read(ref _generation)))
            {
                outcome.Result.Dispose(); // a newer query superseded this one
                return;
            }

            TraceCaptured?.Invoke(outcome.Trace);
            if (outcome.Trace?.Unchanged == true)
            {
                // Identical results (engine-verified): no Reset, no count
                // text churn — idle USN traffic stops repainting the screen.
                await _presenter.RefreshInPlaceAsync(
                    outcome.Result,
                    outcome.Trace,
                    origin,
                    highlighter,
                    () => IsCurrentGeneration(generation, Interlocked.Read(ref _generation)),
                    ct);
                return;
            }

            await _presenter.PublishAsync(
                outcome.Result,
                outcome.Trace,
                origin,
                highlighter,
                () => IsCurrentGeneration(generation, Interlocked.Read(ref _generation)),
                ct);
        }
        catch (OperationCanceledException) when (ct.IsCancellationRequested)
        {
            // Page teardown or a lifetime cancellation: deliberately silent.
        }
        catch (StaleResultException)
        {
            // The index was structurally rebuilt mid-prefetch. Retry once;
            // origin==Stale marks the retry, so a second stale gives up
            // (the next IndexChanged will requery anyway).
            if (origin != RequeryOrigin.Stale)
            {
                Requery(RequeryOrigin.Stale);
            }
            else
            {
                FileLog.WarnEvent(
                    "query",
                    "result stale twice in a row",
                    null,
                    ("qlen", queryLength));
            }
        }
        catch (QuerySyntaxException e)
        {
            _presenter.PresentQueryError(e.Message);
        }
        catch (EngineUnavailableException e)
        {
            // The pipe supervisor is already reconnecting; its synthesized
            // IndexChanged will requery once the service is back.
            FileLog.WarnEvent(
                "query",
                "engine unavailable",
                e,
                ("qlen", queryLength));
            _presenter.PresentEngineFailure();
            SearchFailed?.Invoke(e);
        }
        catch (EngineException e)
        {
            FileLog.ErrorEvent("query", "engine error", e, ("qlen", queryLength));
            _presenter.PresentEngineFailure();
            SearchFailed?.Invoke(e);
        }
        catch (Exception e)
        {
            // Last line of defense: never let a query crash the app silently.
            FileLog.ErrorEvent("query", "unexpected query failure", e, ("qlen", queryLength));
            SearchFailed?.Invoke(e);
        }
    }

    /// <summary>Cancel in-flight engine/page work, stop the debounce timer and
    /// detach both auto-requery subscriptions. Idempotent.</summary>
    public void Dispose()
    {
        if (Interlocked.Exchange(ref _disposed, 1) != 0)
        {
            return;
        }

        Interlocked.Increment(ref _generation);
        _debounce.Stop();
        CancelNoThrow(Interlocked.Exchange(ref _activeQuery, null));
        _lifetime.Cancel();
        _presenter.ResultsSource.BecameStale -= _staleHandler;
        _engineEvents.IndexChanged -= _indexChangedHandler;
        TraceCaptured = null;
        SearchFailed = null;
        _lifetime.Dispose();
    }

    /// <summary>Pure text classification kept separate from fire-and-forget
    /// orchestration so null/empty mutation checks cannot strand an async query.</summary>
    /// <param name="value">Current text-box value.</param>
    /// <returns>True for null or empty input.</returns>
    internal static bool IsEmptyText(string? value) => string.IsNullOrEmpty(value);

    /// <summary>Pure generation comparison used both before and during publish.</summary>
    /// <param name="candidate">Generation captured by the operation.</param>
    /// <param name="current">Latest orchestrator generation.</param>
    /// <returns>True only while the operation is still current.</returns>
    internal static bool IsCurrentGeneration(long candidate, long current) => candidate == current;
}
