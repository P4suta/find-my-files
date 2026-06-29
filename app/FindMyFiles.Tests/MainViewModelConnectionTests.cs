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
        Assert.Equal(0, engine.ListVolumesCalls);

        // The pipe connects — the first Connected event drives the real startup.
        engine.Connection = EngineConnectionState.Connected;
        engine.RaiseConnectionChanged(EngineConnectionState.Connected);
        _dispatcher.DrainQueue();

        Assert.Equal(1, engine.ListVolumesCalls);
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
    public async Task EnableSearchAsync_on_success_relaunches_into_the_pipe()
    {
        var relaunches = 0;
        var provisioner = new ServiceProvisioner(
            register: () => Task.FromResult(ServiceActionOutcome.Ok),
            relaunch: () => relaunches++);
        using var vm = new MainViewModel(
            FakeEngineClient.CreateEmpty(), _dispatcher, new AppSettings(), provisioner: provisioner);

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
            FakeEngineClient.CreateEmpty(), _dispatcher, new AppSettings(), provisioner: provisioner);

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
            FakeEngineClient.CreateEmpty(), _dispatcher, new AppSettings(), provisioner: provisioner);

        await vm.EnableSearchAsync();

        Assert.Equal(0, relaunches);
        Assert.Equal(Loc.Get("Setup_Failed"), vm.SetupStatus);
        Assert.False(vm.SetupBusy);
    }
}
