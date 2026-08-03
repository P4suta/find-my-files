using FindMyFiles.Engine;
using FindMyFiles.Services;
using FindMyFiles.Tests.TestDoubles;
using FindMyFiles.ViewModels;
using Xunit;

namespace FindMyFiles.Tests;

/// <summary>
/// <see cref="MainViewModel"/>'s connection-driven startup — the transition the
/// shipped onboarding bug broke: a pipe that is still warming up at page load must
/// not surface a bogus failure, and the very first Connected event must drive the
/// startup so the UI goes Setup/preparing → Ready on its own. Also the setup
/// screen's one-click <see cref="MainViewModel.EnableSearchAsync"/> outcome
/// branches. Engine state is driven on a <see cref="StubEngineClient"/> and
/// marshalled through the <see cref="ManualDispatcher"/>.
/// </summary>
public sealed class MainViewModelConnectionTests
{
    private readonly ManualDispatcher _dispatcher = new();

    // The volume set the stub's ListVolumesAsync returns (CA1861: not inline).
    private static readonly string[] StubVolumes = ["F:"];

    public MainViewModelConnectionTests() => SyncContext.RunContinuationsInline();

    [Fact]
    public async Task StartAsync_defers_while_connecting_then_runs_once_on_connected()
    {
        var engine = new StubEngineClient { Connection = EngineConnectionState.Connecting };
        using var vm = new MainViewModel(engine, _dispatcher, new AppSettings());

        await vm.StartAsync();

        // Warm-up: held on "preparing", the startup work has NOT run (no bogus
        // "index start failed" from calling a not-yet-connected engine).
        Assert.Equal(Loc.Get("Status_Preparing"), vm.StatusText);
        Assert.False(vm.CanSearch);
        Assert.Equal(Loc.Get("Status_Preparing"), vm.SearchInputPlaceholder);
        Assert.Equal(0, engine.ListVolumesCalls);

        // The pipe connects — the first Connected event drives the real startup.
        engine.Connection = EngineConnectionState.Connected;
        engine.RaiseConnectionChanged(EngineConnectionState.Connected);
        _dispatcher.DrainQueue();

        Assert.Equal(1, engine.ListVolumesCalls);
        Assert.True(vm.CanSearch);
        Assert.Equal(vm.SearchPlaceholder, vm.SearchInputPlaceholder);
        Assert.Equal(
            StatusFormatter.Overall(Array.Empty<VolumeStatus>(), StubVolumes), vm.StatusText);

        // A later Connected (a reconnect) must NOT re-run the startup sequence.
        engine.RaiseConnectionChanged(EngineConnectionState.Connected);
        _dispatcher.DrainQueue();
        Assert.Equal(1, engine.ListVolumesCalls);
    }

    [Fact]
    public async Task StartAsync_runs_immediately_when_already_connected()
    {
        // The fast path (probe succeeded before Loaded, or FFI/in-proc): no deferral.
        var engine = new StubEngineClient { Connection = EngineConnectionState.Connected };
        using var vm = new MainViewModel(engine, _dispatcher, new AppSettings());

        await vm.StartAsync();

        Assert.Equal(1, engine.ListVolumesCalls);
        Assert.Equal(
            StatusFormatter.Overall(Array.Empty<VolumeStatus>(), StubVolumes), vm.StatusText);
    }

    [Fact]
    public async Task StartAsync_waits_for_the_service_version_before_startup_work()
    {
        var stats = new TaskCompletionSource<EngineStatsData?>(
            TaskCreationOptions.RunContinuationsAsynchronously);
        var engine = new StubEngineClient
        {
            Kind = EngineClientKind.Service,
            Connection = EngineConnectionState.Connected,
            StatsTask = stats.Task,
        };
        using var vm = new MainViewModel(engine, _dispatcher, new AppSettings());

        var startup = vm.StartAsync();
        Assert.False(startup.IsCompleted);
        Assert.Equal(0, engine.ListVolumesCalls);

        stats.SetResult(new EngineStatsData
        {
            Service = new ServiceInfoData { Version = BuildInfo.Version },
        });
        await startup;

        Assert.Equal(BuildInfo.Version, vm.EngineVersion);
        Assert.Equal(1, engine.ListVolumesCalls);
        Assert.Equal(1, engine.StartIndexingCalls);
    }

    [Fact]
    public void Connected_from_a_terminal_state_restores_the_search_surface()
    {
        var engine = new StubEngineClient
        {
            Kind = EngineClientKind.Service,
            Connection = EngineConnectionState.Faulted,
            Stats = new EngineStatsData
            {
                Service = new ServiceInfoData { Version = BuildInfo.Version },
            },
        };
        using var vm = new MainViewModel(engine, _dispatcher, new AppSettings());

        // This test owns only the terminal-to-connected transition. Keep a
        // pre-existing query from adding debounce/search work to DrainQueue;
        // MainViewModelTests separately pins the constructor's empty default.
        vm.SearchText = string.Empty;
        Assert.True(vm.IsDisconnected);

        engine.Connection = EngineConnectionState.Connected;
        engine.RaiseConnectionChanged(EngineConnectionState.Connected);
        _dispatcher.DrainQueue();

        Assert.False(vm.IsDisconnected);
        Assert.True(vm.IsReady);
        Assert.True(vm.CanSearch);
        Assert.Equal(BuildInfo.Version, vm.EngineVersion);
        Assert.Equal(1, engine.ListVolumesCalls);
    }

    [Theory]
    [InlineData((int)EngineConnectionState.Connecting)]
    [InlineData((int)EngineConnectionState.Reconnecting)]
    public async Task StartAsync_defers_for_every_recoverable_pipe_warmup_state(
        int stateValue)
    {
        var state = (EngineConnectionState)stateValue;
        var engine = new StubEngineClient { Connection = state };
        using var vm = new MainViewModel(engine, _dispatcher, new AppSettings());

        await vm.StartAsync();

        Assert.True(vm.IsReady);
        Assert.False(vm.CanSearch);
        Assert.Equal(Loc.Get("Status_Preparing"), vm.StatusText);
        Assert.Equal(0, engine.ListVolumesCalls);
    }

    [Theory]
    [InlineData((int)EngineConnectionState.Unavailable)]
    [InlineData((int)EngineConnectionState.Faulted)]
    public async Task StartAsync_routes_every_terminal_transport_state_to_repair(
        int stateValue)
    {
        var state = (EngineConnectionState)stateValue;
        var engine = new StubEngineClient { Connection = state };
        using var vm = new MainViewModel(engine, _dispatcher, new AppSettings());

        await vm.StartAsync();

        Assert.True(vm.IsDisconnected);
        Assert.False(vm.IsReady);
        Assert.False(vm.CanSearch);
        Assert.Equal(Loc.Get("Status_ServiceUnregistered"), vm.StatusText);
        Assert.Equal(0, engine.ListVolumesCalls);
    }

    [Fact]
    public async Task StartAsync_handles_a_terminal_state_that_races_the_constructor()
    {
        var engine = new StubEngineClient { Connection = EngineConnectionState.InProc };
        using var vm = new MainViewModel(engine, _dispatcher, new AppSettings());
        engine.Connection = EngineConnectionState.Faulted;

        await vm.StartAsync();

        Assert.True(vm.IsDisconnected);
        Assert.Equal(Loc.Get("Status_ServiceUnregistered"), vm.StatusText);
        Assert.Equal(0, engine.ListVolumesCalls);
    }

    [Fact]
    public void A_non_pipe_connection_event_only_updates_search_availability()
    {
        var engine = new StubEngineClient { Connection = EngineConnectionState.Connecting };
        using var vm = new MainViewModel(engine, _dispatcher, new AppSettings());

        engine.RaiseConnectionChanged(EngineConnectionState.InProc);
        _dispatcher.DrainQueue();

        Assert.True(vm.CanSearch);
        Assert.False(vm.IsDisconnected);
        Assert.Equal(0, engine.ListVolumesCalls);
    }

    [Fact]
    public async Task EnableSearchAsync_on_success_relaunches_into_the_pipe()
    {
        var relaunches = 0;
        var provisioner = new ServiceProvisioner(
            register: () => Task.FromResult(ServiceActionOutcome.Ok),
            relaunch: () => relaunches++);
        using var vm = new MainViewModel(
            new UnavailableEngineClient(), _dispatcher, new AppSettings(), provisioner: provisioner);

        await vm.EnableSearchAsync();

        Assert.Equal(1, relaunches); // forced pipe relaunch; production would exit here
        Assert.Equal(Loc.Get("Setup_Connecting"), vm.SetupStatus);
        Assert.False(vm.SetupBusy);
    }

    [Fact]
    public async Task EnableSearchAsync_on_cancel_clears_status_without_relaunching()
    {
        var relaunches = 0;
        var provisioner = new ServiceProvisioner(
            register: () => Task.FromResult(ServiceActionOutcome.Cancelled),
            relaunch: () => relaunches++);
        using var vm = new MainViewModel(
            new UnavailableEngineClient(), _dispatcher, new AppSettings(), provisioner: provisioner);

        await vm.EnableSearchAsync();

        Assert.Equal(0, relaunches); // the user dismissed the UAC prompt — no relaunch
        Assert.Equal(string.Empty, vm.SetupStatus);
        Assert.False(vm.SetupBusy);
    }

    [Fact]
    public async Task EnableSearchAsync_on_failure_reports_failed_without_relaunching()
    {
        var relaunches = 0;
        var provisioner = new ServiceProvisioner(
            register: () => Task.FromResult(ServiceActionOutcome.Failed),
            relaunch: () => relaunches++);
        using var vm = new MainViewModel(
            new UnavailableEngineClient(), _dispatcher, new AppSettings(), provisioner: provisioner);

        await vm.EnableSearchAsync();

        Assert.Equal(0, relaunches);
        Assert.Equal(Loc.Get("Setup_Failed"), vm.SetupStatus);
        Assert.False(vm.SetupBusy);
    }

    [Fact]
    public async Task EnableSearchAsync_on_identity_failure_explains_why_Uac_did_not_open()
    {
        var relaunches = 0;
        var provisioner = new ServiceProvisioner(
            register: () => Task.FromResult(ServiceActionOutcome.IdentityUnavailable),
            relaunch: () => relaunches++);
        using var vm = new MainViewModel(
            new UnavailableEngineClient(), _dispatcher, new AppSettings(), provisioner: provisioner);

        await vm.EnableSearchAsync();

        Assert.Equal(0, relaunches);
        Assert.Equal(Loc.Get("Svc_IdentityUnavailable"), vm.SetupStatus);
        Assert.False(vm.SetupBusy);
    }

    [Fact]
    public async Task EnableSearchAsync_rejects_a_second_call_while_Uac_is_pending()
    {
        var completion = new TaskCompletionSource<ServiceActionOutcome>();
        var calls = 0;
        var provisioner = new ServiceProvisioner(
            register: () =>
            {
                calls++;
                return completion.Task;
            },
            relaunch: () => { });
        using var vm = new MainViewModel(
            new UnavailableEngineClient(), _dispatcher, new AppSettings(), provisioner: provisioner);

        var first = vm.EnableSearchAsync();
        var second = vm.EnableSearchAsync();
        var secondCompletedSynchronously = second.IsCompleted;
        var busyWhilePending = vm.SetupBusy;
        var statusWhilePending = vm.SetupStatus;
        completion.SetResult(ServiceActionOutcome.Cancelled);
        await Task.WhenAll(first, second);

        Assert.True(secondCompletedSynchronously);
        Assert.True(busyWhilePending);
        Assert.Equal(Loc.Get("Setup_WaitingForPermission"), statusWhilePending);
        Assert.Equal(1, calls);
        Assert.False(vm.SetupBusy);
    }

    [Fact]
    public async Task EnableSearchAsync_does_not_relaunch_a_disposed_page()
    {
        var completion = new TaskCompletionSource<ServiceActionOutcome>();
        var relaunches = 0;
        var provisioner = new ServiceProvisioner(
            register: () => completion.Task,
            relaunch: () => relaunches++);
        var vm = new MainViewModel(
            new UnavailableEngineClient(), _dispatcher, new AppSettings(), provisioner: provisioner);

        var enable = vm.EnableSearchAsync();
        vm.Dispose();
        completion.SetResult(ServiceActionOutcome.Ok);
        await enable;

        Assert.Equal(0, relaunches);
        Assert.False(vm.SetupBusy);
    }
}
