using FindMyFiles.Engine;
using FindMyFiles.Services;

namespace FindMyFiles.ViewModels;

/// <summary>Snapshot of what to search — the ViewModel stays the single
/// source of truth for the UI state; the orchestrator only pulls it.</summary>
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
    private readonly IDispatcher _dispatcher;
    private readonly ResultsPresenter _presenter;
    private readonly Func<SearchRequest> _request;
    private readonly IDispatcherTimer _debounce;
    private long _generation;

    /// <summary>Stage trace of the last completed query (null when the
    /// engine produced none) — perf-panel food.</summary>
    public event Action<QueryTraceData?>? TraceCaptured;

    /// <summary>Engine or unexpected failure (never query syntax — that goes
    /// to the presenter as count text).</summary>
    public event Action<Exception>? SearchFailed;

    public SearchOrchestrator(
        IEngineClient engine,
        IDispatcher dispatcher,
        ResultsPresenter presenter,
        Func<SearchRequest> request)
    {
        _engine = engine;
        _dispatcher = dispatcher;
        _presenter = presenter;
        _request = request;
        _debounce = dispatcher.CreateOneShotTimer(
            TimeSpan.FromMilliseconds(50),
            () => Requery(RequeryOrigin.Typing));

        _presenter.ResultsSource.BecameStale += () => Requery(RequeryOrigin.Stale);
        _engine.IndexChanged += _ =>
            _dispatcher.TryEnqueue(() => Requery(RequeryOrigin.IndexChanged));
    }

    public void NotifyTextChanged(string value)
    {
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

    public void Requery(RequeryOrigin origin) =>
        RunQueryAsync(origin).Forget($"query.{origin}");

    private async Task RunQueryAsync(RequeryOrigin origin)
    {
        var generation = Interlocked.Increment(ref _generation);
        var request = _request();
        try
        {
            var outcome = await _engine.SearchAsync(request.Query, request.Options);
            if (generation != Interlocked.Read(ref _generation))
            {
                outcome.Result.Dispose(); // a newer query superseded this one
                return;
            }
            TraceCaptured?.Invoke(outcome.Trace);
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
                FileLog.Warn("query", $"result stale twice in a row for `{request.Query}`");
            }
        }
        catch (QuerySyntaxException e)
        {
            _presenter.PresentQueryError(e.Message);
        }
        catch (EngineException e)
        {
            FileLog.Error("query", $"engine error for query `{request.Query}`", e);
            _presenter.PresentEngineFailure();
            SearchFailed?.Invoke(e);
        }
        catch (Exception e)
        {
            // Last line of defense: never let a query crash the app silently.
            FileLog.Error("query", $"unexpected failure for query `{request.Query}`", e);
            SearchFailed?.Invoke(e);
        }
    }
}
