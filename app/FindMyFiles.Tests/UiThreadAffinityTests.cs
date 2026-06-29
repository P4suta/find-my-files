using System.Collections.Concurrent;
using System.Diagnostics;
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
    // The relaunch tests post to the process-wide Notifier via ShellOps.Run's
    // swallowed-failure path; reset it on teardown so a post can't replay into
    // another test's ViewModel (serial execution makes this deterministic).
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
    public async Task RelaunchWith_off_the_ui_thread_loses_a_direct_exit_the_orphaned_window_bug()
    {
        // Models the shipped bug: the post-register relaunch resumes on a pool
        // thread, where a direct Application.Current.Exit() throws and is swallowed.
        using var dispatcher = new DedicatedThreadDispatcher();
        var runner = new CountingProcessRunner();
        var exit = new ThreadAffineExit(dispatcher.ThreadId);

        await Task.Run(() => ShellOps.RelaunchWith(runner, exit)); // off the UI thread

        Assert.Equal(1, runner.Calls); // the new instance started (new window opens)...
        Assert.Equal(0, exit.Exits);   // ...but the exit was lost (old window orphaned)
    }

    [Fact]
    public async Task RelaunchWith_off_the_ui_thread_completes_a_marshaling_exit_on_the_ui_thread()
    {
        // The fix: DispatcherAppExit marshals the exit onto the UI thread, so even
        // when the relaunch resumes on a pool thread the old window closes.
        using var dispatcher = new DedicatedThreadDispatcher();
        var runner = new CountingProcessRunner();
        var exit = new MarshalingAppExit(dispatcher);

        await Task.Run(() => ShellOps.RelaunchWith(runner, exit));
        var ranThreadId = await exit.Ran;

        Assert.Equal(1, runner.Calls);
        Assert.Equal(dispatcher.ThreadId, ranThreadId); // the exit ran on the UI thread
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
            relaunch: () => { }); // no-op: don't exit the test process
        using var vm = new MainViewModel(
            FakeEngineClient.CreateEmpty(), dispatcher, new AppSettings(), provisioner: provisioner);

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

    private sealed class CountingProcessRunner : IProcessRunner
    {
        public int Calls { get; private set; }

        public void Start(ProcessStartInfo psi) => Calls++;
    }

    private sealed class MarshalingAppExit : IAppExit
    {
        private readonly DedicatedThreadDispatcher _dispatcher;
        private readonly TaskCompletionSource<int> _ran = new();

        public MarshalingAppExit(DedicatedThreadDispatcher dispatcher) => _dispatcher = dispatcher;

        public Task<int> Ran => _ran.Task;

        public void Exit() =>
            _dispatcher.TryEnqueue(() => _ran.TrySetResult(Environment.CurrentManagedThreadId));
    }
}
