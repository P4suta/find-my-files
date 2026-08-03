using System.Reflection;
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
    public MainViewModelTests() => SyncContext.RunContinuationsInline();

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
    public void Constructor_normalizes_an_unknown_regex_scope_without_persisting_it()
    {
        var saves = 0;
        var settings = new AppSettings { RegexScope = "obsolete" };
        using var vm = new MainViewModel(
            new FakeEngineClient(),
            new ManualDispatcher(),
            settings,
            saveSettings: () =>
            {
                saves++;
                return true;
            });

        Assert.Equal(RegexScope.Name, vm.RegexScope);
        Assert.Equal("obsolete", settings.RegexScope);
        Assert.Equal(0, saves);
    }

    [Fact]
    public void Persisted_setting_restore_suppresses_regex_scope_normalization_saves()
    {
        var saves = 0;
        var settings = new AppSettings { RegexScope = "name" };
        using var vm = new MainViewModel(
            new FakeEngineClient(),
            new ManualDispatcher(),
            settings,
            saveSettings: () =>
            {
                saves++;
                return true;
            });

        var restore = typeof(MainViewModel).GetMethod(
            "RestorePersistedSetting",
            BindingFlags.Instance | BindingFlags.NonPublic);
        Assert.NotNull(restore);
        restore.Invoke(vm, [(Action)(() => vm.RegexScope = RegexScope.Path)]);

        Assert.Equal(RegexScope.Path, vm.RegexScope);
        Assert.Equal("name", settings.RegexScope);
        Assert.Equal(0, saves);
    }

    [Fact]
    public void Bound_setting_already_at_the_target_is_not_saved_redundantly()
    {
        var saves = 0;
        var settings = new AppSettings { FocusedSearch = false };
        using var vm = new MainViewModel(
            new FakeEngineClient(),
            new ManualDispatcher(),
            settings,
            saveSettings: () =>
            {
                saves++;
                return true;
            });
        settings.FocusedSearch = true;

        vm.FocusedSearch = true;

        Assert.True(vm.FocusedSearch);
        Assert.True(settings.FocusedSearch);
        Assert.Equal(0, saves);
    }

    [Fact]
    public void Constructor_defaults_and_each_disconnected_signal_are_explicit()
    {
        using (var vm = new MainViewModel(
            new StubEngineClient
            {
                Kind = EngineClientKind.Unavailable,
                Connection = EngineConnectionState.InProc,
            },
            new ManualDispatcher(),
            new AppSettings()))
        {
            Assert.True(vm.IsDisconnected);
            Assert.Equal(Loc.Get("Status_Preparing"), vm.StatusText);
            Assert.Equal(Loc.Get("Status_ModePrivileged"), vm.ModeText);
            Assert.Equal(string.Empty, vm.SearchText);
            Assert.Equal(string.Empty, vm.EngineVersion);
            Assert.Equal(string.Empty, vm.SetupStatus);
        }

        using var faulted = new MainViewModel(
            new StubEngineClient
            {
                Kind = EngineClientKind.Service,
                Connection = EngineConnectionState.Faulted,
            },
            new ManualDispatcher(),
            new AppSettings());
        Assert.True(faulted.IsDisconnected);
    }

    [Fact]
    public void Derived_labels_and_placeholders_reflect_every_bound_state()
    {
        using var vm = Vm(new FakeEngineClient());
        vm.SearchText = "needle";
        Assert.Equal(Loc.Get("NoResults_Body", "needle"), vm.NoResultsText);
        Assert.Equal(Loc.Get("Search_Placeholder"), vm.SearchPlaceholder);
        Assert.Equal(vm.SearchPlaceholder, vm.SearchInputPlaceholder);
        Assert.Equal(Loc.Get("About_AppVersion", BuildInfo.Version), MainViewModel.AppVersionText);

        vm.RegexMode = true;
        vm.RegexScope = RegexScope.Name;
        Assert.Equal(Loc.Get("Search_PlaceholderRegexName"), vm.SearchPlaceholder);
        vm.RegexScope = RegexScope.Path;
        Assert.Equal(Loc.Get("Search_PlaceholderRegexPath"), vm.SearchPlaceholder);

        vm.CanSearch = false;
        Assert.Equal(Loc.Get("Status_Preparing"), vm.SearchInputPlaceholder);
        vm.EngineVersion = "1.2.3";
        Assert.Equal(Loc.Get("About_EngineVersion", "1.2.3"), vm.EngineVersionText);
        vm.SetupBusy = true;
        Assert.False(vm.SetupNotBusy);
        vm.SetupBusy = false;
        Assert.True(vm.SetupNotBusy);
    }

    [Fact]
    public async Task StartAsync_on_the_empty_engine_reports_unregistered_and_skips_indexing()
    {
        using var vm = Vm(new UnavailableEngineClient());

        await vm.StartAsync();

        Assert.Equal(Loc.Get("Status_ServiceUnregistered"), vm.StatusText);
    }

    [Fact]
    public async Task Disconnected_kind_short_circuits_even_if_transport_state_looks_usable()
    {
        var engine = new StubEngineClient
        {
            Kind = EngineClientKind.Unavailable,
            Connection = EngineConnectionState.InProc,
        };
        using var vm = new MainViewModel(
            engine,
            new ManualDispatcher(),
            new AppSettings());

        await vm.StartAsync();

        Assert.Equal(0, engine.ListVolumesCalls);
        Assert.Equal(0, engine.StartIndexingCalls);
        Assert.Equal(Loc.Get("Status_ServiceUnregistered"), vm.StatusText);
    }

    [Fact]
    public async Task Startup_indexes_once_and_issues_the_initial_nonempty_query()
    {
        SyncContext.RunContinuationsInline();
        var dispatcher = new ManualDispatcher();
        var engine = new StubEngineClient();
        using var vm = new MainViewModel(
            engine,
            dispatcher,
            new AppSettings { FocusedSearch = false });
        vm.SearchText = "startup";

        await vm.StartAsync();

        Assert.Equal(1, engine.StartIndexingCalls);
        Assert.Equal("startup", Assert.Single(engine.Searches).Query);
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
        Assert.Equal(Loc.Get("Settings_SaveFailedBody"), failure.Detail);
    }

    [Fact]
    public void Failed_setting_restore_requeries_once_after_the_rollback_completes()
    {
        SyncContext.RunContinuationsInline();
        var dispatcher = new ManualDispatcher();
        var engine = new StubEngineClient();
        var settings = new AppSettings { FocusedSearch = true };
        using var vm = new MainViewModel(
            engine,
            dispatcher,
            settings,
            saveSettings: () => false);
        vm.SearchText = "query";
        dispatcher.FireTimers();
        Assert.Single(engine.Searches);

        vm.FocusedSearch = false;

        var searchCount = engine.Searches.Count;
        foreach (var search in engine.Searches.ToArray())
        {
            search.CompleteWith([]);
        }

        dispatcher.DrainQueue();
        Assert.True(vm.FocusedSearch);
        Assert.True(vm.Search.FocusedSearch);
        Assert.Equal(2, searchCount);
    }

    [Fact]
    public void Search_and_tray_settings_persist_successful_changes()
    {
        var saves = 0;
        var settings = new AppSettings();
        using var vm = new MainViewModel(
            new FakeEngineClient(),
            new ManualDispatcher(),
            settings,
            saveSettings: () =>
            {
                saves++;
                return true;
            });

        vm.RegexMode = true;
        vm.RegexScope = RegexScope.Path;
        vm.CloseToTray = true;

        Assert.True(settings.RegexMode);
        Assert.Equal("path", settings.RegexScope);
        Assert.True(settings.CloseToTray);
        Assert.Equal(3, saves);

        vm.RegexScope = RegexScope.Name;
        Assert.Equal("name", settings.RegexScope);
        Assert.Equal(4, saves);
    }

    [Fact]
    public void Search_affecting_changes_requery_with_the_updated_options()
    {
        SyncContext.RunContinuationsInline();
        var dispatcher = new ManualDispatcher();
        var engine = new StubEngineClient();
        using var vm = new MainViewModel(
            engine,
            dispatcher,
            new AppSettings { FocusedSearch = false });
        vm.SearchText = "query";
        dispatcher.FireTimers();
        Assert.Single(engine.Searches);

        vm.IncludeHiddenSystem = true;
        Assert.True(engine.Searches[^1].Options.IncludeHiddenSystem);
        vm.FocusedSearch = true;
        Assert.True(vm.Search.FocusedSearch);
        vm.RegexMode = true;
        Assert.True(engine.Searches[^1].Options.RegexMode);
        vm.RegexScope = RegexScope.Path;
        Assert.Equal(RegexScope.Path, engine.Searches[^1].Options.Scope);
        vm.SetSort(FmfSort.Size);
        Assert.Equal(FmfSort.Size, engine.Searches[^1].Options.Sort);
        vm.SetSortDescending(true);
        Assert.True(engine.Searches[^1].Options.Descending);

        Assert.Equal(7, engine.Searches.Count);
    }

    [Fact]
    public void Regex_scope_is_inert_until_regex_mode_is_enabled()
    {
        SyncContext.RunContinuationsInline();
        var dispatcher = new ManualDispatcher();
        var engine = new StubEngineClient();
        using var vm = new MainViewModel(engine, dispatcher, new AppSettings());
        vm.SearchText = "query";
        dispatcher.FireTimers();

        vm.RegexScope = RegexScope.Path;
        Assert.Single(engine.Searches);
        vm.RegexMode = true;
        Assert.Equal(2, engine.Searches.Count);
        vm.RegexScope = RegexScope.Name;
        Assert.Equal(3, engine.Searches.Count);
    }

    [Fact]
    public void Published_empty_results_and_the_next_edit_drive_the_empty_state()
    {
        SyncContext.RunContinuationsInline();
        var dispatcher = new ManualDispatcher();
        var engine = new StubEngineClient();
        using var vm = new MainViewModel(engine, dispatcher, new AppSettings());
        vm.SearchText = "none";
        dispatcher.FireTimers();

        Assert.Single(engine.Searches).CompleteWith([]);
        Assert.True(vm.HasNoResults);

        vm.SearchText = "next";
        Assert.False(vm.HasNoResults);
    }

    [Fact]
    public void Published_nonempty_results_never_show_the_empty_state()
    {
        SyncContext.RunContinuationsInline();
        var dispatcher = new ManualDispatcher();
        var engine = new StubEngineClient();
        using var vm = new MainViewModel(engine, dispatcher, new AppSettings());
        vm.SearchText = "one";
        dispatcher.FireTimers();

        Assert.Single(engine.Searches).CompleteWith([Rows.File(1, "one.txt")]);

        Assert.False(vm.HasNoResults);
    }

    [Fact]
    public void Completed_query_trace_is_forwarded_to_the_perf_panel()
    {
        SyncContext.RunContinuationsInline();
        var dispatcher = new ManualDispatcher();
        var engine = new StubEngineClient();
        using var vm = new MainViewModel(engine, dispatcher, new AppSettings());
        vm.SearchText = "traced";
        dispatcher.FireTimers();
        var trace = new QueryTraceData { TotalUs = 123 };

        Assert.Single(engine.Searches).CompleteWith([], trace);

        Assert.Same(trace, vm.Perf.LastTrace);
    }

    [Fact]
    public void Failed_regex_and_tray_saves_restore_the_previous_values()
    {
        var settings = new AppSettings
        {
            RegexMode = false,
            RegexScope = "name",
            CloseToTray = false,
        };
        using var vm = new MainViewModel(
            new FakeEngineClient(),
            new ManualDispatcher(),
            settings,
            saveSettings: () => false);

        vm.RegexMode = true;
        vm.RegexScope = RegexScope.Path;
        vm.CloseToTray = true;

        Assert.False(vm.RegexMode);
        Assert.Equal(RegexScope.Name, vm.RegexScope);
        Assert.False(vm.CloseToTray);
        Assert.False(settings.RegexMode);
        Assert.Equal("name", settings.RegexScope);
        Assert.False(settings.CloseToTray);
        Assert.Equal(3, vm.Notifications.Items.Count);
    }

    [Fact]
    public void Failed_regex_scope_save_restores_a_previous_path_scope()
    {
        var settings = new AppSettings { RegexScope = "path" };
        using var vm = new MainViewModel(
            new FakeEngineClient(),
            new ManualDispatcher(),
            settings,
            saveSettings: () => false);

        vm.RegexScope = RegexScope.Name;

        Assert.Equal(RegexScope.Path, vm.RegexScope);
        Assert.Equal("path", settings.RegexScope);
    }

    [Fact]
    public void Explicit_sort_direction_requeries_only_on_a_change()
    {
        SyncContext.RunContinuationsInline();
        var dispatcher = new ManualDispatcher();
        var engine = new StubEngineClient();
        using var vm = new MainViewModel(engine, dispatcher, new AppSettings());
        vm.SearchText = "sorted";
        dispatcher.FireTimers();

        vm.SetSortDescending(false);
        Assert.False(vm.SortDescending);
        Assert.Single(engine.Searches);
        vm.SetSortDescending(true);
        Assert.True(vm.SortDescending);
        Assert.Equal(2, engine.Searches.Count);
    }

    [Fact]
    public void Dispose_detaches_every_owned_engine_event_handler()
    {
        var vm = new MainViewModel(
            new StubEngineClient(),
            new ManualDispatcher(),
            new AppSettings());
        var field = typeof(MainViewModel).GetField(
            "_engineEvents",
            BindingFlags.Instance | BindingFlags.NonPublic);
        var events = Assert.IsType<EngineEventMarshaler>(field?.GetValue(vm));

        try
        {
            Assert.NotNull(GetHandler(events, nameof(EngineEventMarshaler.VolumeUpdated)));
            Assert.NotNull(GetHandler(events, nameof(EngineEventMarshaler.EngineErrorOccurred)));
            Assert.NotNull(GetHandler(events, nameof(EngineEventMarshaler.ConnectionChanged)));

            vm.Dispose();

            Assert.Null(GetHandler(events, nameof(EngineEventMarshaler.VolumeUpdated)));
            Assert.Null(GetHandler(events, nameof(EngineEventMarshaler.EngineErrorOccurred)));
            Assert.Null(GetHandler(events, nameof(EngineEventMarshaler.ConnectionChanged)));
        }
        finally
        {
            vm.Dispose();
        }

        static Delegate? GetHandler(EngineEventMarshaler source, string eventName) =>
            typeof(EngineEventMarshaler).GetField(
                eventName,
                BindingFlags.Instance | BindingFlags.NonPublic)
                ?.GetValue(source) as Delegate;
    }

    [Theory]
    [InlineData(false, 3, 3, true)]
    [InlineData(true, 3, 3, false)]
    [InlineData(false, 2, 3, false)]
    public void Version_refresh_is_applied_only_for_the_current_live_owner(
        bool disposed,
        int candidate,
        int current,
        bool expected) =>
        Assert.Equal(expected, MainViewModel.ShouldApplyVersion(disposed, candidate, current));

    private static StubEngineClient EngineReportingVersion(string serviceVersion) =>
        new()
        {
            Kind = EngineClientKind.Service,
            Connection = EngineConnectionState.Connected,
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
    public void RefreshVersions_resumes_on_the_captured_ui_context()
    {
        var completion = new TaskCompletionSource<EngineStatsData?>(
            TaskCreationOptions.RunContinuationsAsynchronously);
        var engine = EngineReportingVersion("99.0.0");
        engine.StatsTask = completion.Task;
        using var vm = Vm(engine);
        var ui = new RecordingSyncContext();
        var previous = SynchronizationContext.Current;
        SynchronizationContext.SetSynchronizationContext(ui);
        try
        {
            var refresh = vm.RefreshVersionsAsync();
            Assert.False(refresh.IsCompleted);

            SynchronizationContext.SetSynchronizationContext(null);
            completion.SetResult(new EngineStatsData
            {
                Service = new ServiceInfoData { Version = BuildInfo.Version },
            });
            SynchronizationContext.SetSynchronizationContext(ui);

            Assert.True(ui.Posted > 0);
            Assert.Equal(string.Empty, vm.EngineVersion);
            ui.Drain();
            Assert.True(refresh.IsCompletedSuccessfully);
            Assert.Equal(BuildInfo.Version, vm.EngineVersion);
        }
        finally
        {
            SynchronizationContext.SetSynchronizationContext(previous);
        }
    }

    [Fact]
    public async Task RefreshVersions_flags_a_mismatch_when_the_base_differs()
    {
        using var vm = Vm(EngineReportingVersion("99.0.0-dev+gabc1234"));

        await vm.RefreshVersionsAsync();

        Assert.True(vm.HasEngineVersion);
        Assert.True(vm.HasVersionMismatch);
        var warning = Assert.Single(vm.Notifications.Items);
        Assert.Equal(NotifySeverity.Warning, warning.Severity);
        Assert.Equal(Loc.GetXaml("AboutVersionMismatch", "Title"), warning.Message);
        Assert.Equal(Loc.GetXaml("AboutVersionMismatch", "Message"), warning.Detail);
    }

    [Fact]
    public async Task Version_mismatch_warning_is_unique_actionable_and_clears_when_repaired()
    {
        var repairs = 0;
        var engine = EngineReportingVersion("99.0.0-dev+gabc1234");
        using var vm = new MainViewModel(
            engine,
            new ManualDispatcher(),
            new AppSettings(),
            openServiceManager: () => repairs++);

        await vm.RefreshVersionsAsync();
        await vm.RefreshVersionsAsync();

        var warning = Assert.Single(vm.Notifications.Items);
        Assert.Equal(Loc.Get("VersionMismatch_RepairAction"), warning.ActionLabel);
        Assert.Equal("VersionMismatchRepair", warning.ActionAutomationId);
        warning.Invoke();
        Assert.Equal(1, repairs);

        engine.Stats!.Service!.Version = BuildInfo.Version;
        await vm.RefreshVersionsAsync();

        Assert.False(vm.HasVersionMismatch);
        Assert.Empty(vm.Notifications.Items);
    }

    [Fact]
    public void Reconnect_refreshes_service_version_without_duplicating_the_warning()
    {
        var dispatcher = new ManualDispatcher();
        var engine = EngineReportingVersion("99.0.0");
        engine.Connection = EngineConnectionState.Connecting;
        using var vm = new MainViewModel(engine, dispatcher, new AppSettings());
        vm.SearchText = string.Empty;

        engine.Connection = EngineConnectionState.Connected;
        engine.RaiseConnectionChanged(EngineConnectionState.Connected);
        dispatcher.DrainQueue();
        engine.Connection = EngineConnectionState.Reconnecting;
        engine.RaiseConnectionChanged(EngineConnectionState.Reconnecting);
        dispatcher.DrainQueue();
        engine.Connection = EngineConnectionState.Connected;
        engine.RaiseConnectionChanged(EngineConnectionState.Connected);
        dispatcher.DrainQueue();

        Assert.True(vm.HasVersionMismatch);
        Assert.Single(vm.Notifications.Items);
    }

    [Fact]
    public async Task RefreshVersions_stays_empty_for_in_proc_clients_without_a_service()
    {
        // Even misleading stats must be ignored for in-proc/Fake clients: there
        // is no separately installed service whose build can be repaired.
        using var vm = Vm(new StubEngineClient
        {
            Stats = new EngineStatsData
            {
                Service = new ServiceInfoData { Version = "99.0.0" },
            },
        });

        await vm.RefreshVersionsAsync();

        Assert.False(vm.HasEngineVersion);
        Assert.False(vm.HasVersionMismatch);
        Assert.Empty(vm.Notifications.Items);
    }

    [Fact]
    public async Task In_process_refresh_clears_a_previously_visible_service_version()
    {
        using var vm = Vm(new StubEngineClient());
        vm.EngineVersion = "99.0.0";

        await vm.RefreshVersionsAsync();

        Assert.Equal(string.Empty, vm.EngineVersion);
        Assert.False(vm.HasVersionMismatch);
    }

    [Fact]
    public async Task RefreshVersions_stays_empty_for_the_fake_engine()
    {
        using var vm = Vm(new FakeEngineClient());

        await vm.RefreshVersionsAsync();

        Assert.False(vm.HasEngineVersion);
        Assert.False(vm.HasVersionMismatch);
        Assert.Empty(vm.Notifications.Items);
    }

    [Fact]
    public async Task RefreshVersions_logs_and_stays_empty_when_stats_fail()
    {
        using var log = new LogCapture();
        using var vm = Vm(new StubEngineClient
        {
            Kind = EngineClientKind.Service,
            Connection = EngineConnectionState.Connected,
            ThrowOnStats = new EngineUnavailableException("offline"),
        });

        vm.EngineVersion = "99.0.0";
        var error = await Record.ExceptionAsync(vm.RefreshVersionsAsync);

        Assert.Null(error);
        Assert.False(vm.HasEngineVersion);
        Assert.False(vm.HasVersionMismatch);
        Assert.Empty(vm.Notifications.Items);
        Assert.Contains("area=engine", log.Text, StringComparison.Ordinal);
        Assert.Contains("engine version unavailable", log.Text, StringComparison.Ordinal);
        Assert.Contains(
            "error_type=FindMyFiles.Engine.EngineUnavailableException",
            log.Text,
            StringComparison.Ordinal);
    }

    [Fact]
    public async Task RefreshVersions_treats_missing_service_stats_as_no_version()
    {
        using var vm = Vm(new StubEngineClient
        {
            Kind = EngineClientKind.Service,
            Connection = EngineConnectionState.Connected,
            Stats = null,
        });

        await vm.RefreshVersionsAsync();

        Assert.Equal(string.Empty, vm.EngineVersion);
        Assert.False(vm.HasEngineVersion);
        Assert.False(vm.HasVersionMismatch);
        Assert.Empty(vm.Notifications.Items);
    }

    [Fact]
    public void Include_hidden_system_changes_are_forwarded_to_search()
    {
        using var vm = Vm(new FakeEngineClient());

        vm.IncludeHiddenSystem = true;

        Assert.True(vm.IncludeHiddenSystem);
    }

    [Fact]
    public async Task RefreshVersions_ignores_a_stale_success_and_failure()
    {
        var first = new TaskCompletionSource<EngineStatsData?>();
        var engine = EngineReportingVersion("99.0.0");
        engine.StatsTask = first.Task;
        using var vm = Vm(engine);

        var stale = vm.RefreshVersionsAsync();
        engine.StatsTask = Task.FromResult<EngineStatsData?>(new EngineStatsData
        {
            Service = new ServiceInfoData { Version = BuildInfo.Version },
        });
        await vm.RefreshVersionsAsync();
        first.SetException(new EngineUnavailableException("old session"));
        await stale;

        Assert.Equal(BuildInfo.Version, vm.EngineVersion);
        Assert.False(vm.HasVersionMismatch);
    }

    [Fact]
    public async Task RefreshVersions_cancellation_after_dispose_is_quiet()
    {
        var pending = new TaskCompletionSource<EngineStatsData?>();
        var engine = EngineReportingVersion("99.0.0");
        engine.StatsTask = pending.Task;
        var vm = Vm(engine);

        var refresh = vm.RefreshVersionsAsync();
        vm.Dispose();
        pending.SetException(new OperationCanceledException());

        Assert.Null(await Record.ExceptionAsync(async () => await refresh));
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

    [Fact]
    public async Task Dispose_cancels_and_disposes_owned_async_lifetimes()
    {
        var pending = new TaskCompletionSource<EngineStatsData?>();
        var engine = EngineReportingVersion("99.0.0");
        engine.StatsTask = pending.Task;
        var vm = Vm(engine);
        var version = vm.RefreshVersionsAsync();
        var perf = vm.Perf.RefreshStatsAsync();
        Assert.Equal(2, engine.StatsTokens.Count);
        var lifetime = Assert.IsAssignableFrom<CancellationTokenSource>(
            typeof(MainViewModel)
                .GetField("_lifetime", BindingFlags.Instance | BindingFlags.NonPublic)!
                .GetValue(vm));

        vm.Dispose();

        Assert.All(engine.StatsTokens, token => Assert.True(token.IsCancellationRequested));
        Assert.Throws<ObjectDisposedException>(() => _ = lifetime.Token);
        pending.SetException(new OperationCanceledException());
        await Task.WhenAll(version, perf).WaitAsync(TimeSpan.FromSeconds(2));
    }

    [Fact]
    public void Constructor_attaches_and_dispose_detaches_the_global_notifier()
    {
        Notifier.ResetForTests();
        try
        {
            var vm = Vm(new FakeEngineClient());
            Assert.Equal(1, NotifierSubscriberCount());

            vm.Dispose();

            Assert.Equal(0, NotifierSubscriberCount());
        }
        finally
        {
            Notifier.ResetForTests();
        }
    }

    private static int NotifierSubscriberCount() =>
        (typeof(Notifier)
            .GetField("_posted", BindingFlags.Static | BindingFlags.NonPublic)!
            .GetValue(null) as Delegate)?.GetInvocationList().Length ?? 0;
}
