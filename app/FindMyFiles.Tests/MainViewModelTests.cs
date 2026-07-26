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
    public void Unavailable_engine_shows_the_disconnected_setup_state()
    {
        using var vm = Vm(new UnavailableEngineClient());

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
        using var vm = Vm(new UnavailableEngineClient());

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

    [Fact]
    public void Failed_settings_save_reverts_the_bound_value_and_surfaces_an_error()
    {
        var settings = new AppSettings { FocusedSearch = true };
        using var vm = new MainViewModel(
            new FakeEngineClient(),
            new ManualDispatcher(),
            settings,
            saveSettings: () => false);

        vm.FocusedSearch = false;

        Assert.True(vm.FocusedSearch);
        Assert.True(settings.FocusedSearch);
        Assert.True(vm.Search.FocusedSearch);
        var failure = Assert.Single(vm.Notifications.Items);
        Assert.Equal(NotifySeverity.Error, failure.Severity);
        Assert.Equal(Loc.Get("Settings_SaveFailedTitle"), failure.Message);
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

    [Fact]
    public void Dispose_is_idempotent_cancels_search_and_detaches_engine_events()
    {
        SyncContext.RunContinuationsInline();
        var dispatcher = new ManualDispatcher();
        var engine = new StubEngineClient();
        var vm = new MainViewModel(engine, dispatcher, new AppSettings());
        vm.SearchText = "report";
        dispatcher.FireTimers();
        var pending = Assert.Single(engine.Searches);

        vm.Dispose();
        vm.Dispose();

        Assert.True(pending.CancellationToken.IsCancellationRequested);
        Assert.Equal(0, engine.IndexChangedSubscribers);
        Assert.Equal(0, engine.VolumeUpdatedSubscribers);
        Assert.Equal(0, engine.EngineErrorSubscribers);
        Assert.Equal(0, engine.ConnectionChangedSubscribers);

        engine.RaiseIndexChanged("C:");
        dispatcher.DrainQueue();
        Assert.Single(engine.Searches);
    }

    [Fact]
    public void Dispose_releases_the_published_result_handle()
    {
        SyncContext.RunContinuationsInline();
        var dispatcher = new ManualDispatcher();
        var engine = new StubEngineClient();
        var vm = new MainViewModel(engine, dispatcher, new AppSettings());
        vm.SearchText = "report";
        dispatcher.FireTimers();
        var result = engine.Searches[0].CompleteWith(Rows.Many(3));
        Assert.False(result.Disposed);

        vm.Dispose();

        Assert.True(result.Disposed);
    }
}
