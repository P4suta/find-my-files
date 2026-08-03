using System.Collections.Concurrent;
using FindMyFiles.Engine;
using FindMyFiles.Services;
using FindMyFiles.Tests.TestDoubles;
using FindMyFiles.ViewModels;
using Xunit;

namespace FindMyFiles.Tests;

/// <summary>
/// Regression tests for the UI-thread-affinity bug class that shipped the
/// orphaned-window setup-screen bug. Built on <see cref="DedicatedThreadDispatcher"/>
/// (real thread identity); <see cref="ManualDispatcher"/> (HasThreadAccess always
/// true) structurally cannot catch any of these.
/// </summary>
public sealed class UiThreadAffinityTests : IDisposable
{
    // EnableSearchAsync can route a failure to the process-wide Notifier; reset it
    // on teardown so a post can't replay into another test (serial execution makes
    // this deterministic).
    public void Dispose()
    {
        Notifier.ResetForTests();
        GC.SuppressFinalize(this);
    }

    [Fact]
    public async Task Dispatcher_reports_thread_access_only_on_its_own_thread()
    {
        using var dispatcher = new DedicatedThreadDispatcher();

        Assert.False(dispatcher.HasThreadAccess); // the test thread is not the pump thread

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
        // The VM await must not ConfigureAwait(false): bound SetupStatus/SetupBusy
        // writes after the elevated register (which completes on a pool thread) must
        // resume on the UI thread, or WinUI throws RPC_E_WRONG_THREAD at runtime.
        using var dispatcher = new DedicatedThreadDispatcher();
        var provisioner = new ServiceProvisioner(
            register: async () =>
            {
                // A genuine suspension that resumes on a pool thread, so the VM's
                // await is exercised for real: a task that completes instantly would
                // resume inline on the pump thread and hide a ConfigureAwait(false)
                // regression (the bug only bites when the await actually suspends).
                await Task.Delay(20).ConfigureAwait(false);
                return ServiceActionOutcome.Ok;
            },
            relaunch: () => { }); // no-op: don't rebuild the page in the test
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
        // The hottest UI-affine path in the app: SearchOrchestrator awaits the
        // engine, then the presenter awaits page reads, and only then does it
        // mutate the ListView's ItemsSource (Reassign → Reset), the bound
        // CountText and the bound perf trace. Both awaits complete off the UI
        // thread in production (FFI Task.Run / pipe read loop), so every one of
        // them is a place where ConfigureAwait(false) would move XAML mutations
        // onto a pool thread. ManualDispatcher cannot see any of it: its
        // HasThreadAccess is true everywhere, which also neuters
        // VirtualResultList.EnsureUiThread, the production guard for this.
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
                // Also releases the wait: a cross-thread mutation aborts the query
                // instead of publishing, and the assertion below should report the
                // real exception rather than a timeout.
                failures.Enqueue(e);
                published.TrySetResult();
            };
            vm.Results.ResultsPublished += _ =>
            {
                Record("results.published");
                published.TrySetResult();
            };

            vm.SearchText = "row"; // debounce → requery → engine → prefetch → publish
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

        // Two independent witnesses: the recorded thread identity of every bound
        // mutation, and the production guard itself — EnsureUiThread throws into
        // the orchestrator's catch-all, which surfaces as SearchFailed.
        Assert.Empty(failures);
        Assert.Empty(offThread);
        Assert.Null(dispatcher.LastError);
        Assert.Equal(150, vm.Results.ResultsSource.Count);
    }

    [Fact]
    public async Task Startup_failure_writes_status_and_notifications_on_the_ui_thread()
    {
        // RunStartupAsync resumes after three engine round-trips and then writes
        // the bound StatusText and pushes into the bound notification collection.
        // The failure branch is the interesting one: it mutates an
        // ObservableCollection, which throws inside XAML from a pool thread.
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

        // The continuation really ran (otherwise "nothing off-thread" is vacuous).
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
        // The F12 poll runs once a second for as long as the panel is open, and
        // its continuation writes bound Stats and raises PerfDataChanged, which
        // the view turns into imperative XAML drawing.
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

        Assert.NotNull(perf.Stats); // the continuation really ran
        Assert.Empty(offThread);
        Assert.Null(dispatcher.LastError);
    }
}
