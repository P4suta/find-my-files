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

    private static StubEngineClient EngineReportingVersion(string serviceVersion) =>
        new()
        {
            Stats = new EngineStatsData
            {
                Service = new ServiceInfoData { Version = serviceVersion },
            },
        };

    [Fact]
    public async Task RefreshVersions_exposes_the_engine_version_and_clears_mismatch_on_same_base()
    {
        // Same X.Y.Z base as the app (different channel/sha) → no mismatch.
        string sameBase = $"{BuildInfo.BaseOf(BuildInfo.Version)}-nightly.20260629+gabc1234";
        using var vm = Vm(EngineReportingVersion(sameBase));

        await vm.RefreshVersionsAsync();

        Assert.True(vm.HasEngineVersion);
        Assert.Equal(sameBase, vm.EngineVersion);
        Assert.False(vm.HasVersionMismatch);
    }

    [Fact]
    public async Task RefreshVersions_flags_a_mismatch_when_the_base_differs()
    {
        using var vm = Vm(EngineReportingVersion("99.0.0-dev+gabc1234"));

        await vm.RefreshVersionsAsync();

        Assert.True(vm.HasEngineVersion);
        Assert.True(vm.HasVersionMismatch);
    }

    [Fact]
    public async Task RefreshVersions_stays_empty_for_in_proc_clients_without_a_service()
    {
        // Stub with no stats → in-proc client (Ffi/Fake): no separate service.
        using var vm = Vm(new StubEngineClient());

        await vm.RefreshVersionsAsync();

        Assert.False(vm.HasEngineVersion);
        Assert.False(vm.HasVersionMismatch);
    }
}
