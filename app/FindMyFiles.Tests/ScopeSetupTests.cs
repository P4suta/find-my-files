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
    private static readonly string[] FoldersA = [@"C:\A"];
    private static readonly string[] FoldersAOther = [@"C:\A", @"C:\Other"];
    private static readonly string[] FoldersDocs = [@"C:\Docs"];
    private static readonly string[] FoldersB = [@"C:\B"];

    private MainViewModel Build(
        AppSettings settings,
        Func<Task<string?>>? picker = null,
        Action? relaunch = null,
        Func<bool>? isScopeMode = null)
    {
        SyncContext.RunContinuationsInline();
        return new MainViewModel(
            _engine,
            _dispatcher,
            settings,
            picker ?? (() => Task.FromResult<string?>(null)),
            relaunch ?? (() => { }),
            isScopeMode);
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

    [Fact]
    public void ApplyScopeChange_relaunches_and_persists_normalized()
    {
        var relaunched = false;
        var settings = new AppSettings(); // unconfigured (mirrors a fresh setup)
        var vm = Build(settings, relaunch: () => relaunched = true);
        vm.ScopeFolders.Add(@"C:\A");
        vm.ScopeFolders.Add(@"C:\A\B"); // nested under C:\A → collapsed away
        vm.ScopeFolders.Add(@"C:\Other");

        vm.ApplyScopeChange();

        Assert.True(relaunched);
        Assert.Equal(FoldersAOther, settings.ScopeRoots);
    }

    [Fact]
    public void ApplyScopeChange_is_a_noop_when_unchanged_ignoring_order_and_case()
    {
        var relaunched = false;
        var settings = new AppSettings { ScopeRoots = [@"C:\A", @"C:\B"] };
        var vm = Build(settings, relaunch: () => relaunched = true);
        vm.ScopeFolders.Clear();
        vm.ScopeFolders.Add(@"c:\b"); // same set, different order + case
        vm.ScopeFolders.Add(@"C:\A");

        vm.ApplyScopeChange();

        Assert.False(relaunched);
        Assert.Equal(FoldersAB, settings.ScopeRoots); // original casing untouched
    }

    [Fact]
    public void ApplyScopeChange_is_a_noop_when_nesting_collapses_to_the_current_set()
    {
        var relaunched = false;
        var settings = new AppSettings { ScopeRoots = [@"C:\A"] };
        var vm = Build(settings, relaunch: () => relaunched = true);
        vm.ScopeFolders.Add(@"C:\A\B"); // collapses back to {C:\A}

        vm.ApplyScopeChange();

        Assert.False(relaunched);
        Assert.Equal(FoldersA, settings.ScopeRoots);
    }

    [Fact]
    public void ApplyScopeChange_is_a_noop_with_no_folders()
    {
        var relaunched = false;
        var vm = Build(new AppSettings(), relaunch: () => relaunched = true);

        vm.ApplyScopeChange();

        Assert.False(relaunched);
    }

    [Fact]
    public void IsScopeMode_drives_the_mode_split_when_ready()
    {
        var vm = Build(new AppSettings(), isScopeMode: () => true);

        Assert.True(vm.IsReady); // StubEngineClient is not the empty fake
        Assert.True(vm.IsScopeMode);
        Assert.False(vm.IsPrivilegedMode);
    }

    [Fact]
    public void IsPrivilegedMode_drives_the_mode_split_when_ready()
    {
        var vm = Build(new AppSettings(), isScopeMode: () => false);

        Assert.True(vm.IsPrivilegedMode);
        Assert.False(vm.IsScopeMode);
    }

    [Fact]
    public void ModeText_differs_between_the_two_modes()
    {
        var scope = Build(new AppSettings(), isScopeMode: () => true).ModeText;
        var privileged = Build(new AppSettings(), isScopeMode: () => false).ModeText;

        Assert.NotEqual(scope, privileged);
    }
}
