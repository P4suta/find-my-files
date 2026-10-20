using System.Collections.Concurrent;
using FindMyFiles.Engine;
using FindMyFiles.Services;
using FindMyFiles.Tests.TestDoubles;
using FindMyFiles.ViewModels;
using Xunit;

namespace FindMyFiles.Tests;

/// <summary> Regression tests for the UI-thread-affinity bug class that shipped the orphaned-window setup-screen bug.
/// Built on <see cref="DedicatedThreadDispatcher"/> (real thread identity); <see cref="ManualDispatcher"/> (HasThreadAccess always true) structurally cannot catch any of these.
/// </summary>
public sealed class UiThreadAffinityTests : IDisposable
{
    public void Dispose()
    {
        Notifier.ResetForTests();
        GC.SuppressFinalize(this);
    }

    [Fact]
    public async Task Dispatcher_reports_thread_access_only_on_its_own_thread()
    {
        using var dispatcher = new DedicatedThreadDispatcher();

        Assert.False(dispatcher.HasThreadAccess);

        var hadAccessOnThread = await dispatcher.InvokeAsync(() =>
            Task.FromResult(dispatcher.HasThreadAccess));
        var ranThreadId = await dispatcher.InvokeAsync(() =>
            Task.FromResult(Environment.CurrentManagedThreadId));

        Assert.True(hadAccessOnThread);
        Assert.Equal(dispatcher.ThreadId, ranThreadId);
    }

    [Fact]
    public async Task EnableSearchAsync_keeps_bound_writes_on_the_ui_thread()
    {
        using var dispatcher = new DedicatedThreadDispatcher();
        var provisioner = new ServiceProvisioner(
            register: async () =>
            {
                await Task.Delay(20).ConfigureAwait(false);
                return ServiceActionOutcome.Ok;
            },
            relaunch: () => { });
        using var vm = new MainViewModel(
            new UnavailableEngineClient(), dispatcher, new AppSettings(), provisioner: provisioner);

        var offThreadWrites = new ConcurrentQueue<string?>();
        vm.PropertyChanged += (_, e) =>
        {
            if (Environment.CurrentManagedThreadId != dispatcher.ThreadId)
            {
                offThreadWrites.Enqueue(e.PropertyName);
            }
        };

        await dispatcher.InvokeAsync(() => vm.EnableSearchAsync());

        Assert.Empty(offThreadWrites);
    }

    [Fact]
    public async Task Search_pipeline_publishes_results_only_on_the_ui_thread()
    {
        using var dispatcher = new DedicatedThreadDispatcher();
        using var engine = new OffThreadEngineClient(rowCount: 150);
        var offThread = new ConcurrentQueue<string>();
        var failures = new ConcurrentQueue<Exception>();
        var published = new TaskCompletionSource(
            TaskCreationOptions.RunContinuationsAsynchronously);
        MainViewModel vm = null!;

        void Record(string what)
        {
            if (Environment.CurrentManagedThreadId != dispatcher.ThreadId)
            {
                offThread.Enqueue($"{what}@{Environment.CurrentManagedThreadId}");
            }
        }

        await dispatcher.InvokeAsync(() =>
        {
            vm = new MainViewModel(engine, dispatcher, new AppSettings());
            vm.PropertyChanged += (_, e) => Record($"vm.{e.PropertyName}");
            vm.Perf.PropertyChanged += (_, e) => Record($"perf.{e.PropertyName}");
            vm.Results.PropertyChanged += (_, e) => Record($"results.{e.PropertyName}");
            vm.Results.ResultsSource.CollectionChanged += (_, _) => Record("results.reset");
            vm.Search.SearchFailed += e =>
            {
                failures.Enqueue(e);
                published.TrySetResult();
            };
            vm.Results.ResultsPublished += _ =>
            {
                Record("results.published");
                published.TrySetResult();
            };

            vm.SearchText = "row";
            return Task.CompletedTask;
        });

        try
        {
            await published.Task.WaitAsync(TimeSpan.FromSeconds(2));
        }
        finally
        {
            if (vm is not null)
            {
                await dispatcher.InvokeAsync(() =>
                {
                    vm.Dispose();
                    return Task.CompletedTask;
                });
            }
        }

        Assert.Empty(failures);
        Assert.Empty(offThread);
        Assert.Null(dispatcher.LastError);
        Assert.Equal(150, vm.Results.ResultsSource.Count);
    }

    [Fact]
    public async Task Startup_failure_writes_status_and_notifications_on_the_ui_thread()
    {
        using var dispatcher = new DedicatedThreadDispatcher();
        using var engine = new OffThreadEngineClient
        {
            ThrowOnStartup = new InvalidOperationException("volumes unavailable"),
        };
        var offThread = new ConcurrentQueue<string>();
        MainViewModel vm = null!;

        void Record(string what)
        {
            if (Environment.CurrentManagedThreadId != dispatcher.ThreadId)
            {
                offThread.Enqueue($"{what}@{Environment.CurrentManagedThreadId}");
            }
        }

        await dispatcher.InvokeAsync(async () =>
        {
            vm = new MainViewModel(engine, dispatcher, new AppSettings());
            vm.PropertyChanged += (_, e) => Record($"vm.{e.PropertyName}");
            vm.Notifications.Items.CollectionChanged += (_, _) => Record("notifications");
            await vm.StartAsync();
        });

        Assert.Single(vm.Notifications.Items);
        Assert.Empty(offThread);
        Assert.Null(dispatcher.LastError);

        await dispatcher.InvokeAsync(() =>
        {
            vm.Dispose();
            return Task.CompletedTask;
        });
    }

    [Fact]
    public async Task Perf_stats_poll_updates_bound_state_on_the_ui_thread()
    {
        using var dispatcher = new DedicatedThreadDispatcher();
        using var engine = new OffThreadEngineClient { Stats = new EngineStatsData() };
        using var perf = new PerfPanelViewModel(engine);
        var offThread = new ConcurrentQueue<string>();

        void Record(string what)
        {
            if (Environment.CurrentManagedThreadId != dispatcher.ThreadId)
            {
                offThread.Enqueue($"{what}@{Environment.CurrentManagedThreadId}");
            }
        }

        perf.PropertyChanged += (_, e) => Record($"perf.{e.PropertyName}");
        perf.PerfDataChanged += () => Record("perf.dataChanged");

        await dispatcher.InvokeAsync(() => perf.RefreshStatsAsync());

        Assert.NotNull(perf.Stats);
        Assert.Empty(offThread);
        Assert.Null(dispatcher.LastError);
    }
}
