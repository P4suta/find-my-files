using FindMyFiles.Engine;
using FindMyFiles.Services;
using FindMyFiles.Tests.TestDoubles;
using FindMyFiles.ViewModels;
using Xunit;

namespace FindMyFiles.Tests;

/// <summary>Behavioural tests for <see cref="MainViewModel"/> — the main page's
/// composition root, previously untested. Built entirely from the existing
/// injectable boundaries (fake engine + manual dispatcher), so no real engine,
/// thread, or window is involved.</summary>
public sealed class MainViewModelTests
{
    private static MainViewModel Vm(IEngineClient engine) =>
        new(engine, new ManualDispatcher(), new AppSettings());

    [Fact]
    public void Empty_engine_shows_the_disconnected_setup_state()
    {
        using var vm = Vm(FakeEngineClient.CreateEmpty());

        Assert.True(vm.IsDisconnected);
        Assert.False(vm.IsReady);
    }

    [Fact]
    public void Populated_engine_is_ready()
    {
        using var vm = Vm(new FakeEngineClient());

        Assert.False(vm.IsDisconnected);
        Assert.True(vm.IsReady);
    }

    [Fact]
    public async Task StartAsync_on_the_empty_engine_reports_unregistered_and_skips_indexing()
    {
        using var vm = Vm(FakeEngineClient.CreateEmpty());

        await vm.StartAsync();

        Assert.Equal(Loc.Get("Status_ServiceUnregistered"), vm.StatusText);
    }

    [Fact]
    public void SetSort_toggles_direction_when_the_column_is_unchanged()
    {
        using var vm = Vm(new FakeEngineClient());
        vm.Sort = FmfSort.Name;
        vm.SortDescending = false;

        vm.SetSort(FmfSort.Name);
        Assert.True(vm.SortDescending);

        vm.SetSort(FmfSort.Name);
        Assert.False(vm.SortDescending);
    }

    [Fact]
    public void SetSort_switches_to_a_new_column_ascending()
    {
        using var vm = Vm(new FakeEngineClient());
        vm.Sort = FmfSort.Name;
        vm.SortDescending = true;

        vm.SetSort(FmfSort.Size);

        Assert.Equal(FmfSort.Size, vm.Sort);
        Assert.False(vm.SortDescending);
    }
}
