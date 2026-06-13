using FindMyFiles.Engine;
using FindMyFiles.Services;

namespace FindMyFiles.ViewModels;

/// <summary>Snapshot of what to search — the ViewModel stays the single
/// source of truth for the UI state; the orchestrator only pulls it.</summary>
/// <param name="Query">Raw user query text (before any focused-mode rewrite).</param>
/// <param name="Options">Sort, case and hidden/system flags for this search.</param>
public readonly record struct SearchRequest(string Query, SearchOptions Options);

/// <summary>
/// When and what to search: 50ms debounce on typing (clearing is instant), a
/// generation counter that discards superseded responses, requery triggers
/// (index changes, stale results) and exception classification. Results are
/// handed to the <see cref="ResultsPresenter"/>; failures surface through
/// <see cref="SearchFailed"/> so the ViewModel owns the user-facing wording.
/// All entry points run on the UI thread.
/// </summary>
public sealed class SearchOrchestrator
{
    private readonly IEngineClient _engine;
    private readonly ResultsPresenter _presenter;
    private readonly Func<SearchRequest> _request;
    private readonly IDispatcherTimer _debounce;
    private long _generation;

    /// <summary>絞り込みモード: when on, the user's query is rewritten with
    /// the two lists below (<see cref="FocusedQueryRewriter"/>) right before
    /// it reaches the engine. Defaults to off here — product wiring
    /// (MainViewModel) pushes the persisted settings in; a toggle flip is a
    /// filter change, so the owner requeries with
    /// <see cref="RequeryOrigin.Filter"/> (top reset).</summary>
    public bool FocusedSearch { get; set; }

    /// <summary>Noise paths excluded in focused mode (settings-owned).</summary>
    public IReadOnlyList<string> FocusedExcludePaths { get; set; } = [];

    /// <summary>Extension whitelist for focused mode (settings-owned).</summary>
    public IReadOnlyList<string> FocusedExtensions { get; set; } = [];

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
        _presenter = presenter;
        _request = request;
        _debounce = dispatcher.CreateOneShotTimer(
            TimeSpan.FromMilliseconds(50),
            () => Requery(RequeryOrigin.Typing));

        _presenter.ResultsSource.BecameStale += () => Requery(RequeryOrigin.Stale);
        // Already on the UI thread — the marshaler is the crossing point.
        engineEvents.IndexChanged += _ => Requery(RequeryOrigin.IndexChanged);
    }

    private bool _composing;

    /// <summary>Search box text changed: debounce a normal edit (50ms),
    /// requery immediately on a clear (so emptying feels instant), and ignore
    /// edits while an IME composition is in flight.</summary>
    public void NotifyTextChanged(string value)
    {
        if (_composing)
        {
            return; // IME composition in flight — wait for the commit
        }
        if (string.IsNullOrEmpty(value))
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
        _composing = true;
        _debounce.Stop();
    }

    /// <summary>IME composition committed (or cancelled) — search the final
    /// text through the normal debounce.</summary>
    public void NotifyCompositionEnded(string value)
    {
        _composing = false;
        NotifyTextChanged(value);
    }

    /// <summary>Fire-and-forget a query for the current UI state, bumping the
    /// generation so any in-flight older response is discarded.
    /// <paramref name="origin"/> records why (and lets the presenter decide
    /// whether to preserve scroll/selection).</summary>
    public void Requery(RequeryOrigin origin) =>
        RunQueryAsync(origin).Forget($"query.{origin}");

    private async Task RunQueryAsync(RequeryOrigin origin)
    {
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
        // keeps the user's text, the engine sees the effective query, and
        // every log/error below reports what the engine actually saw.
        var query = FocusedSearch
            ? FocusedQueryRewriter.Compose(request.Query, FocusedExcludePaths, FocusedExtensions)
            : request.Query;
        try
        {
            var outcome = await _engine.SearchAsync(query, request.Options);
            if (generation != Interlocked.Read(ref _generation))
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
                    () => generation == Interlocked.Read(ref _generation));
                return;
            }
            await _presenter.PublishAsync(
                outcome.Result,
                outcome.Trace,
                origin,
                () => generation == Interlocked.Read(ref _generation));
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
                FileLog.Warn("query", $"result stale twice in a row for `{query}`");
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
            FileLog.Warn("query", $"engine unavailable for query `{query}`: {e.Message}");
            _presenter.PresentEngineFailure();
            SearchFailed?.Invoke(e);
        }
        catch (EngineException e)
        {
            FileLog.Error("query", $"engine error for query `{query}`", e);
            _presenter.PresentEngineFailure();
            SearchFailed?.Invoke(e);
        }
        catch (Exception e)
        {
            // Last line of defense: never let a query crash the app silently.
            FileLog.Error("query", $"unexpected failure for query `{query}`", e);
            SearchFailed?.Invoke(e);
        }
    }
}
