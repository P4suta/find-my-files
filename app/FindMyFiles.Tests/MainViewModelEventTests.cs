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
public sealed class MainViewModelEventTests
{
    private readonly ManualDispatcher _dispatcher = new();
    private readonly StubEngineClient _engine = new();
    private readonly MainViewModel _vm;

    // The volume set the stub's ListVolumesAsync returns (CA1861: not inline).
    private static readonly string[] StubVolumes = ["F:"];

    public MainViewModelEventTests()
    {
        SyncContext.RunContinuationsInline();
        _vm = new MainViewModel(_engine, _dispatcher, new AppSettings());
    }

    [Fact]
    public async Task StartAsync_indexes_and_sets_the_overall_status()
    {
        await _vm.StartAsync();

        Assert.Equal(
            StatusFormatter.Overall(Array.Empty<VolumeStatus>(), StubVolumes),
            _vm.StatusText);
    }

    [Fact]
    public async Task StartAsync_failure_reports_status_and_notifies()
    {
        _engine.ThrowOnStartup = new EngineException("boom", 6);

        await _vm.StartAsync();

        Assert.Equal(Loc.Get("Status_IndexStartFailed"), _vm.StatusText);
        Assert.Contains(_vm.Notifications.Items, n => n.Severity == NotifySeverity.Error);
    }

    [Fact]
    public void Reconnecting_then_connected_shows_then_clears_a_single_banner()
    {
        _engine.RaiseConnectionChanged(EngineConnectionState.Reconnecting);
        _dispatcher.DrainQueue();
        Assert.Single(_vm.Notifications.Items);
        Assert.Equal(NotifySeverity.Warning, _vm.Notifications.Items[0].Severity);

        // A second Reconnecting must not duplicate the held banner.
        _engine.RaiseConnectionChanged(EngineConnectionState.Reconnecting);
        _dispatcher.DrainQueue();
        Assert.Single(_vm.Notifications.Items);

        _engine.RaiseConnectionChanged(EngineConnectionState.Connected);
        _dispatcher.DrainQueue();
        Assert.Empty(_vm.Notifications.Items);
    }

    [Fact]
    public void A_failed_volume_pushes_an_error_notification()
    {
        _engine.RaiseVolumeUpdated(new VolumeStatus("C:", VolumeState.Failed, 0));
        _dispatcher.DrainQueue();

        Assert.Contains(_vm.Notifications.Items, n => n.Severity == NotifySeverity.Error);
    }

    [Theory]
    [InlineData(3)]  // FMF_E_NOT_ADMIN
    [InlineData(5)]  // FMF_E_QUERY_SYNTAX
    [InlineData(7)]  // FMF_E_LOCKED
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
}
