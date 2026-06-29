using FindMyFiles.Engine;
using FindMyFiles.Services;
using FindMyFiles.Tests.TestDoubles;
using FindMyFiles.ViewModels;
using Xunit;

namespace FindMyFiles.Tests;

/// <summary>Behavioural tests for <see cref="MainViewModel"/>'s
/// <c>OnSearchFailed</c> across engine connection states. A surfaced
/// search-failure InfoBar must be suppressed while the connection is still
/// settling (Connecting/Reconnecting already explain the gap via the
/// "preparing" / reconnect banner), yet surfaced once the engine is usable
/// (Connected/InProc). The failure is driven end-to-end: the stub's
/// <see cref="StubEngineClient.ThrowOnSearch"/> makes the debounced requery
/// raise the orchestrator's <c>SearchFailed</c>, which the view model handles
/// on the (manual) UI thread.</summary>
public sealed class MainViewModelSearchFailureTests
{
    private readonly ManualDispatcher _dispatcher = new();
    private readonly StubEngineClient _engine = new();
    private readonly MainViewModel _vm;

    public MainViewModelSearchFailureTests()
    {
        SyncContext.RunContinuationsInline();
        _vm = new MainViewModel(_engine, _dispatcher, new AppSettings());
    }

    /// <summary>Type a non-empty query and let the debounce elapse so the
    /// orchestrator issues the (failing) search. The stub throws synchronously,
    /// so <c>SearchFailed</c> → <c>OnSearchFailed</c> runs inline on the manual
    /// dispatcher's "UI thread".</summary>
    private void DriveSearchFailure(EngineConnectionState connection, Exception failure)
    {
        _engine.Connection = connection;
        _engine.ThrowOnSearch = failure;
        _vm.SearchText = "report"; // arms the 50ms debounce
        _dispatcher.FireTimers();  // interval elapses → requery → SearchAsync throws
    }

    private bool HasErrorNotification =>
        _vm.Notifications.Items.Any(n => n.Severity == NotifySeverity.Error);

    [Theory]
    [InlineData(EngineConnectionState.Reconnecting)]
    [InlineData(EngineConnectionState.Connecting)]
    public void Settling_connection_suppresses_the_search_failure_notification(
        EngineConnectionState connection)
    {
        DriveSearchFailure(connection, new EngineException("boom", 7));

        // The reconnect banner / "preparing" state already explains the gap, so
        // a search that merely raced the connect must not stack an error on top.
        Assert.DoesNotContain(_vm.Notifications.Items, n => n.Severity == NotifySeverity.Error);
    }

    [Theory]
    [InlineData(EngineConnectionState.InProc)]
    [InlineData(EngineConnectionState.Connected)]
    public void Usable_connection_surfaces_the_search_failure_notification(
        EngineConnectionState connection)
    {
        DriveSearchFailure(connection, new EngineException("boom", 7));

        Assert.Contains(_vm.Notifications.Items, n => n.Severity == NotifySeverity.Error);
    }

    [Fact]
    public void Known_engine_failure_uses_the_search_failed_title()
    {
        DriveSearchFailure(EngineConnectionState.Connected, new EngineException("locked", 7));

        var error = Assert.Single(_vm.Notifications.Items, n => n.Severity == NotifySeverity.Error);
        Assert.Equal(Loc.Get("Notify_SearchFailedTitle"), error.Message);
        Assert.NotEqual(Loc.Get("Notify_SearchUnexpectedTitle"), error.Message);
    }

    [Fact]
    public void Unknown_failure_uses_the_unexpected_title()
    {
        DriveSearchFailure(
            EngineConnectionState.Connected, new InvalidOperationException("unexpected"));

        var error = Assert.Single(_vm.Notifications.Items, n => n.Severity == NotifySeverity.Error);
        Assert.Equal(Loc.Get("Notify_SearchUnexpectedTitle"), error.Message);
        Assert.NotEqual(Loc.Get("Notify_SearchFailedTitle"), error.Message);
    }

    [Fact]
    public void Known_failure_detail_appends_the_localized_text_and_engine_message()
    {
        DriveSearchFailure(EngineConnectionState.InProc, new EngineException("raw detail", 7));

        var error = Assert.Single(_vm.Notifications.Items, n => n.Severity == NotifySeverity.Error);

        // Known failures carry both the localized error text (FMF_E_LOCKED here)
        // and the engine's English detail, joined on a newline — the app absorbs
        // the service's English-only surface for diagnostics.
        var localized = MainViewModel.EngineErrorText(new EngineException("raw detail", 7));
        Assert.Equal($"{localized}\nraw detail", error.Detail);
    }

    [Fact]
    public void Suppressed_failure_still_clears_the_no_results_empty_state()
    {
        // Seed a live query, then assert the empty state independently of the
        // SearchText setter (which also clears it): a background USN requery is
        // what fails here, so HasNoResults can only be reset by OnSearchFailed.
        _vm.SearchText = "report";
        _vm.HasNoResults = true; // a prior query had landed empty
        _engine.Connection = EngineConnectionState.Reconnecting;
        _engine.ThrowOnSearch = new EngineException("boom", 7);

        _engine.RaiseIndexChanged("F:"); // idle USN tick → marshaled requery
        _dispatcher.DrainQueue();         // …which fails inside RunQueryAsync

        // OnSearchFailed clears HasNoResults before the suppression branch — an
        // error must never leave the "no results" empty state showing, even when
        // the InfoBar itself is suppressed during reconnect.
        Assert.False(_vm.HasNoResults);
        Assert.False(HasErrorNotification);
    }
}
