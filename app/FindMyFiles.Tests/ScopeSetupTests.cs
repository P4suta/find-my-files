using FindMyFiles.Services;
using FindMyFiles.Tests.TestDoubles;
using FindMyFiles.ViewModels;
using Xunit;

namespace FindMyFiles.Tests;

/// <summary>Unit tests for the scope-mode (ADR-0024) setup logic on
/// <see cref="MainViewModel"/>: the folder list, case-insensitive dedupe,
/// start-button enablement, and the persist-then-relaunch path — with the
/// folder picker and relaunch injected as fakes so no dialog shows and the
/// process never exits.</summary>
public sealed class ScopeSetupTests
{
    private readonly ManualDispatcher _dispatcher = new();
    private readonly StubEngineClient _engine = new();

    // CA1861: constant array arguments to Assert.Equal are hoisted to fields.
    private static readonly string[] FoldersAB = [@"C:\A", @"C:\B"];
    private static readonly string[] FoldersDocs = [@"C:\Docs"];
    private static readonly string[] FoldersB = [@"C:\B"];

    private MainViewModel Build(
        AppSettings settings,
        Func<Task<string?>>? picker = null,
        Action? relaunch = null)
    {
        SyncContext.RunContinuationsInline();
        return new MainViewModel(
            _engine,
            _dispatcher,
            settings,
            picker ?? (() => Task.FromResult<string?>(null)),
            relaunch ?? (() => { }));
    }

    [Fact]
    public void ScopeFolders_seed_from_settings()
    {
        var vm = Build(new AppSettings { ScopeRoots = [@"C:\A", @"C:\B"] });

        Assert.Equal(FoldersAB, vm.ScopeFolders);
        Assert.True(vm.CanStartScope);
    }

    [Fact]
    public void CanStartScope_is_false_with_no_folders()
    {
        var vm = Build(new AppSettings());

        Assert.Empty(vm.ScopeFolders);
        Assert.False(vm.CanStartScope);
    }

    [Fact]
    public async Task PickScopeFolders_adds_the_chosen_folder()
    {
        var vm = Build(new AppSettings(), picker: () => Task.FromResult<string?>(@"C:\Docs"));

        await vm.PickScopeFoldersAsync();

        Assert.Equal(FoldersDocs, vm.ScopeFolders);
        Assert.True(vm.CanStartScope);
    }

    [Fact]
    public async Task PickScopeFolders_ignores_cancel()
    {
        var vm = Build(new AppSettings(), picker: () => Task.FromResult<string?>(null));

        await vm.PickScopeFoldersAsync();

        Assert.Empty(vm.ScopeFolders);
    }

    [Fact]
    public async Task PickScopeFolders_dedupes_case_insensitively()
    {
        var vm = Build(
            new AppSettings { ScopeRoots = [@"C:\Docs"] },
            picker: () => Task.FromResult<string?>(@"c:\docs"));

        await vm.PickScopeFoldersAsync();

        Assert.Single(vm.ScopeFolders);
    }

    /// <summary>Regression (the CI-bundle "can't proceed" bug): the picker is a
    /// genuinely async OS dialog that completes off the UI thread, so its
    /// continuation must post back to the captured UI SynchronizationContext —
    /// not resume inline off it. Adding to the bound <c>ScopeFolders</c> off the
    /// UI thread drives the start button's <c>IsEnabled</c> from a pool thread →
    /// <c>COMException 0x8001010E</c> (RPC_E_WRONG_THREAD), which <c>.Forget</c>
    /// swallows, so the user silently can't proceed. A <c>ConfigureAwait(false)</c>
    /// on the picker await reintroduces it; this pins the fix.</summary>
    [Fact]
    public void PickScopeFolders_resumes_on_the_captured_synchronization_context()
    {
        var picked = new TaskCompletionSource<string?>();
        var vm = Build(new AppSettings(), picker: () => picked.Task); // Build nulls the context

        var ui = new RecordingSyncContext();
        var saved = SynchronizationContext.Current;
        SynchronizationContext.SetSynchronizationContext(ui);
        try
        {
            var pick = vm.PickScopeFoldersAsync(); // suspends at `await _folderPicker()`, capturing `ui`
            Assert.Empty(vm.ScopeFolders);

            // Complete the dialog while `ui` is NOT the current context (null here),
            // mirroring the real WinRT picker completing off the UI thread: the
            // captured `ui` differs from the completer, so a correct continuation
            // must Post back to `ui` rather than run inline.
            SynchronizationContext.SetSynchronizationContext(null);
            picked.SetResult(@"C:\Docs");
            SynchronizationContext.SetSynchronizationContext(ui);

            // With the bug (.ConfigureAwait(false)) the continuation ran inline off
            // `ui`: ScopeFolders is already populated and nothing was marshaled.
            Assert.True(ui.Posted > 0, "picker continuation must marshal to the UI context");
            Assert.Empty(vm.ScopeFolders); // not added until the UI context pumps

            ui.Drain();

            Assert.True(pick.IsCompletedSuccessfully);
            Assert.Equal(FoldersDocs, vm.ScopeFolders);
            Assert.True(vm.CanStartScope);
        }
        finally
        {
            SynchronizationContext.SetSynchronizationContext(saved);
        }
    }

    [Fact]
    public void RemoveScopeFolder_drops_one()
    {
        var vm = Build(new AppSettings { ScopeRoots = [@"C:\A", @"C:\B"] });

        vm.RemoveScopeFolder(@"C:\A");

        Assert.Equal(FoldersB, vm.ScopeFolders);
    }

    [Fact]
    public void StartScopeSearch_persists_roots_and_relaunches()
    {
        var relaunched = false;
        var settings = new AppSettings { ScopeRoots = [@"C:\A"] };
        var vm = Build(settings, relaunch: () => relaunched = true);
        vm.ScopeFolders.Add(@"C:\B");

        vm.StartScopeSearch();

        Assert.Equal(FoldersAB, settings.ScopeRoots);
        Assert.True(relaunched);
    }

    [Fact]
    public void StartScopeSearch_is_a_noop_with_no_folders()
    {
        var relaunched = false;
        var vm = Build(new AppSettings(), relaunch: () => relaunched = true);

        vm.StartScopeSearch();

        Assert.False(relaunched);
    }

    /// <summary>A deferred <see cref="SynchronizationContext"/> that records how
    /// many continuations were marshaled to it (<see cref="Post"/>) and runs them
    /// only when the test pumps (<see cref="Drain"/>) — the message-loop behavior
    /// a real UI dispatcher provides, made deterministic for the test.</summary>
    private sealed class RecordingSyncContext : SynchronizationContext
    {
        private readonly Queue<(SendOrPostCallback Callback, object? State)> _queue = new();

        /// <summary>Continuations marshaled here via <see cref="Post"/>.</summary>
        public int Posted { get; private set; }

        public override void Post(SendOrPostCallback d, object? state)
        {
            Posted++;
            _queue.Enqueue((d, state));
        }

        public void Drain()
        {
            var previous = Current;
            SetSynchronizationContext(this);
            try
            {
                while (_queue.Count > 0)
                {
                    var (callback, state) = _queue.Dequeue();
                    callback(state);
                }
            }
            finally
            {
                SetSynchronizationContext(previous);
            }
        }
    }
}
