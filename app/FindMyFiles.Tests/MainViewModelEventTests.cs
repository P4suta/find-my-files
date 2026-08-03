using FindMyFiles.Engine;
using FindMyFiles.Services;
using FindMyFiles.Tests.TestDoubles;
using FindMyFiles.ViewModels;
using Xunit;

namespace FindMyFiles.Tests;

/// <summary>Behavioural tests for <see cref="MainViewModel"/>'s engine-event and
/// error handling — the startup sequence, reconnect banner, failed-volume and
/// error-code paths that the happy-path constructor tests do not reach. Engine
/// events are raised on the stub and marshalled through the manual dispatcher.</summary>
public sealed class MainViewModelEventTests : IDisposable
{
    private readonly ManualDispatcher _dispatcher = new();
    private readonly StubEngineClient _engine = new();
    private readonly MainViewModel _vm;

    // The volume set the stub's ListVolumesAsync returns (CA1861: not inline).
    private static readonly string[] StubVolumes = ["F:"];

    public MainViewModelEventTests()
    {
        Notifier.ResetForTests();
        SyncContext.RunContinuationsInline();
        _vm = new MainViewModel(_engine, _dispatcher, new AppSettings());
        _vm.SearchText = string.Empty;
    }

    [Fact]
    public async Task StartAsync_indexes_and_sets_the_overall_status()
    {
        await _vm.StartAsync();

        Assert.Equal(
            StatusFormatter.Overall(Array.Empty<VolumeStatus>(), StubVolumes),
            _vm.StatusText);
        Assert.Empty(_vm.Notifications.Items);
        Assert.Equal(1, _engine.StartIndexingCalls);
    }

    [Fact]
    public async Task StartAsync_is_idempotent_after_success()
    {
        await _vm.StartAsync();
        await _vm.StartAsync();

        Assert.Equal(1, _engine.ListVolumesCalls);
    }

    [Fact]
    public async Task Startup_cancellation_after_dispose_is_quiet()
    {
        var pending = new TaskCompletionSource<IReadOnlyList<string>>();
        _engine.ListVolumesTask = pending.Task;

        var startup = _vm.StartAsync();
        _vm.Dispose();
        pending.SetException(new OperationCanceledException());

        Assert.Null(await Record.ExceptionAsync(async () => await startup));
    }

    [Fact]
    public async Task StartAsync_failure_reports_status_and_notifies()
    {
        using var log = new LogCapture();
        _engine.ThrowOnStartup = new EngineException("boom", 6);
        _vm.SearchText = "must-not-run";

        await _vm.StartAsync();

        Assert.Equal(Loc.Get("Status_IndexStartFailed"), _vm.StatusText);
        var failure = Assert.Single(
            _vm.Notifications.Items,
            n => n.Severity == NotifySeverity.Error);
        Assert.Equal(Loc.Get("Common_Retry"), failure.ActionLabel);
        Assert.Equal(Loc.Get("Notify_IndexStartFailedTitle"), failure.Message);
        Assert.Equal("boom", failure.Detail);
        Assert.Contains("area=engine", log.Text, StringComparison.Ordinal);
        Assert.Contains("startup indexing failed", log.Text, StringComparison.Ordinal);
        Assert.Contains("error_type=FindMyFiles.Engine.EngineException", log.Text, StringComparison.Ordinal);
        Assert.Empty(_engine.Searches);

        _engine.ThrowOnStartup = null;
        var retry = new TaskCompletionSource<IReadOnlyList<string>>(
            TaskCreationOptions.RunContinuationsAsynchronously);
        _engine.ListVolumesTask = retry.Task;
        failure.Invoke();
        Assert.DoesNotContain(failure, _vm.Notifications.Items);
        retry.SetResult(StubVolumes);
        var expectedStatus = StatusFormatter.Overall(Array.Empty<VolumeStatus>(), StubVolumes);
        await Polling.WaitUntilAsync(
            () => _engine.ListVolumesCalls == 2
                && string.Equals(_vm.StatusText, expectedStatus, StringComparison.Ordinal)
                && _vm.Notifications.Items.Count == 0,
            "startup retry completion",
            timeoutMs: 2000);
        Assert.Equal(
            expectedStatus,
            _vm.StatusText);
        Assert.Empty(_vm.Notifications.Items);
    }

    [Fact]
    public async Task Startup_retry_observer_failure_reports_the_retry_diagnostic_area()
    {
        _engine.ThrowOnStartup = new EngineException("initial failure", 6);
        await _vm.StartAsync();
        var failure = Assert.Single(_vm.Notifications.Items);
        _vm.StatusText = "retry pending";

        System.ComponentModel.PropertyChangedEventHandler throwOnRetryStatus = (_, args) =>
        {
            if (string.Equals(
                args.PropertyName,
                nameof(MainViewModel.StatusText),
                StringComparison.Ordinal))
            {
                throw new InvalidOperationException("retry status observer failed");
            }
        };
        _vm.PropertyChanged += throwOnRetryStatus;
        try
        {
            failure.Invoke();
        }
        finally
        {
            _vm.PropertyChanged -= throwOnRetryStatus;
        }

        _dispatcher.DrainQueue();
        var diagnostic = Assert.Single(_vm.Notifications.Items);
        Assert.Equal(
            Loc.Get("Crash_InternalArea", "engine.startup-retry"),
            diagnostic.Message);
        Assert.Equal("retry status observer failed", diagnostic.Detail);
    }

    [Fact]
    public async Task StartAsync_failure_allows_a_later_connected_event_to_retry()
    {
        _engine.ThrowOnStartup = new EngineException("boom", 6);
        await _vm.StartAsync();
        Assert.Equal(1, _engine.ListVolumesCalls);

        _engine.ThrowOnStartup = null;
        _engine.RaiseConnectionChanged(EngineConnectionState.Connected);
        _dispatcher.DrainQueue();

        Assert.True(SpinWait.SpinUntil(
            () => _engine.ListVolumesCalls == 2,
            TimeSpan.FromSeconds(2)));
        Assert.Equal(
            StatusFormatter.Overall(Array.Empty<VolumeStatus>(), StubVolumes),
            _vm.StatusText);
        Assert.Empty(_vm.Notifications.Items);
    }

    [Fact]
    public async Task Repeated_startup_failures_replace_the_existing_notification()
    {
        _engine.ThrowOnStartup = new EngineException("first", 6);
        await _vm.StartAsync();
        var first = Assert.Single(_vm.Notifications.Items);

        _engine.ThrowOnStartup = new EngineException("second", 6);
        _engine.RaiseConnectionChanged(EngineConnectionState.Connected);
        _dispatcher.DrainQueue();
        Assert.True(SpinWait.SpinUntil(
            () => _engine.ListVolumesCalls == 2,
            TimeSpan.FromSeconds(2)));

        var second = Assert.Single(_vm.Notifications.Items);
        Assert.NotSame(first, second);
        Assert.Equal("second", second.Detail);
    }

    [Fact]
    public void Reconnecting_then_connected_shows_then_clears_a_single_banner()
    {
        _engine.RaiseConnectionChanged(EngineConnectionState.Reconnecting);
        _dispatcher.DrainQueue();
        Assert.Single(_vm.Notifications.Items);
        Assert.False(_vm.CanSearch);
        var reconnecting = _vm.Notifications.Items[0];
        Assert.Equal(NotifySeverity.Warning, reconnecting.Severity);
        Assert.Equal(Loc.Get("Notify_ReconnectingTitle"), reconnecting.Message);
        Assert.Equal(Loc.Get("Notify_ReconnectingBody"), reconnecting.Detail);

        // A second Reconnecting must not duplicate the held banner.
        _engine.RaiseConnectionChanged(EngineConnectionState.Reconnecting);
        _dispatcher.DrainQueue();
        Assert.Single(_vm.Notifications.Items);

        _engine.RaiseConnectionChanged(EngineConnectionState.Connected);
        _dispatcher.DrainQueue();
        Assert.Empty(_vm.Notifications.Items);
        Assert.True(_vm.CanSearch);
    }

    [Fact]
    public void Reconnecting_then_terminal_fault_does_not_claim_it_is_still_retrying()
    {
        _vm.EngineVersion = "99.0.0";
        _engine.RaiseConnectionChanged(EngineConnectionState.Reconnecting);
        _dispatcher.DrainQueue();
        Assert.Single(_vm.Notifications.Items);

        _engine.RaiseConnectionChanged(EngineConnectionState.Faulted);
        _dispatcher.DrainQueue();
        Assert.Empty(_vm.Notifications.Items);
        Assert.True(_vm.IsDisconnected);
        Assert.False(_vm.IsReady);
        Assert.False(_vm.CanSearch);
        Assert.Equal(string.Empty, _vm.EngineVersion);
    }

    [Fact]
    public void A_failed_volume_pushes_an_error_notification()
    {
        _engine.RaiseVolumeUpdated(new VolumeStatus("C:", VolumeState.Failed, 0));
        _dispatcher.DrainQueue();

        var failure = Assert.Single(_vm.Notifications.Items);
        Assert.Equal(NotifySeverity.Error, failure.Severity);
        Assert.Equal(Loc.Get("Notify_VolumeIndexFailedTitle", "C:"), failure.Message);
        Assert.Equal(Loc.Get("Notify_VolumeIndexFailedBody"), failure.Detail);
    }

    [Fact]
    public void A_ready_volume_updates_status_and_requeries_without_a_notification()
    {
        _vm.SearchText = "ready";
        _dispatcher.FireTimers();
        Assert.Single(_engine.Searches);
        _engine.RaiseVolumeUpdated(new VolumeStatus("C:", VolumeState.Ready, 42));
        _dispatcher.DrainQueue();

        Assert.Equal(
            StatusFormatter.Volume(
                new VolumeStatus("C:", VolumeState.Ready, 42),
                Loc.Get("Status_Preparing")),
            _vm.StatusText);
        Assert.Empty(_vm.Notifications.Items);
        Assert.Equal(2, _engine.Searches.Count);
        _engine.Searches[1].CompleteWith([]);
        _engine.Searches[0].CompleteWith([]);
    }

    [Fact]
    public void Engine_diagnostics_notify_only_for_error_and_panic()
    {
        _engine.Stats = new EngineStatsData();
        _engine.RaiseEngineError(EngineErrorSeverity.Warn);
        _dispatcher.DrainQueue();
        Assert.Empty(_vm.Notifications.Items);

        _engine.RaiseEngineError(EngineErrorSeverity.Error);
        _dispatcher.DrainQueue();
        var error = Assert.Single(_vm.Notifications.Items);
        Assert.Equal(Loc.Get("Notify_EngineErrorTitle"), error.Message);
        Assert.Null(error.Detail);

        _engine.Stats = new EngineStatsData
        {
            RecentErrors =
            [
                new ErrorEventData
                {
                    Area = "query",
                    Message = new string('x', 250),
                },
            ],
        };
        _engine.RaiseEngineError(EngineErrorSeverity.Panic);
        _dispatcher.DrainQueue();

        var panic = _vm.Notifications.Items.Last();
        Assert.Equal(Loc.Get("Notify_EnginePanicTitle"), panic.Message);
        Assert.Equal($"[query] {new string('x', 200)}…", panic.Detail);

        _engine.Stats = new EngineStatsData
        {
            RecentErrors =
            [
                new ErrorEventData
                {
                    Area = "query",
                    Message = new string('y', 200),
                },
            ],
        };
        _engine.RaiseEngineError(EngineErrorSeverity.Error);
        _dispatcher.DrainQueue();
        Assert.Equal($"[query] {new string('y', 200)}", _vm.Notifications.Items.Last().Detail);
    }

    [Fact]
    public void Engine_diagnostics_do_not_publish_after_the_page_is_disposed()
    {
        var pending = new TaskCompletionSource<EngineStatsData?>();
        _engine.StatsTask = pending.Task;
        _engine.RaiseEngineError(EngineErrorSeverity.Error);
        _dispatcher.DrainQueue();

        _vm.Dispose();
        pending.SetResult(new EngineStatsData());

        Assert.Empty(_vm.Notifications.Items);
    }

    [Theory]
    [InlineData(2)] // FMF_E_STALE
    [InlineData(3)] // FMF_E_NOT_ADMIN
    [InlineData(4)] // FMF_E_VOLUME
    [InlineData(5)] // FMF_E_QUERY_SYNTAX
    [InlineData(6)] // FMF_E_IO
    [InlineData(7)] // FMF_E_LOCKED
    [InlineData(99)] // FMF_E_PANIC
    [InlineData(42)] // unknown → generic
    public void EngineErrorText_maps_every_engine_code_to_a_localized_message(int code)
    {
        var text = MainViewModel.EngineErrorText(new EngineException("detail", code));

        Assert.False(string.IsNullOrEmpty(text));
    }

    [Fact]
    public void EngineErrorText_maps_typed_exceptions()
    {
        Assert.Equal(
            Loc.Get("Err_QuerySyntax"),
            MainViewModel.EngineErrorText(new QuerySyntaxException("x")));
        Assert.Equal(
            Loc.Get("Err_Stale"),
            MainViewModel.EngineErrorText(new StaleResultException()));
        Assert.Equal(
            Loc.Get("Err_Generic"),
            MainViewModel.EngineErrorText(new InvalidOperationException("x")));
    }

    public void Dispose()
    {
        _vm.Dispose();
        Notifier.ResetForTests();
    }
}
