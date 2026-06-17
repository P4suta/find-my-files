using System.Diagnostics;
using FindMyFiles.Services;
using Xunit;

namespace FindMyFiles.Tests;

public sealed class ShellOpsTests : IDisposable
{
    // The relaunch failure-path tests below post to the process-wide Notifier
    // (ShellOps.Run reports swallowed failures); reset it on teardown so a post
    // can't replay into another test's ViewModel. DisableTestParallelization
    // (see TestParallelization.cs) makes this reset deterministic.
    public void Dispose()
    {
        Notifier.ResetForTests();
        GC.SuppressFinalize(this);
    }

    [Theory]
    [InlineData(@"C:\Users\Public\report.txt")]
    [InlineData(@"C:\My Documents\quarterly report.txt")] // spaces
    [InlineData("C:\\dir\\name with \" quote.txt")] // a Win32-reserved quote — the MFT scan can surface it
    [InlineData(@"C:\dir\a,b /root C:\Windows.txt")] // comma + space + switch-looking text
    [InlineData("C:\\dir\\\" /select,C:\\Windows\\System32\\calc.exe")] // an explorer-switch injection payload
    public void BuildOpenStartInfo_PassesPathAsOneVerbatimArgument(string fullPath)
    {
        var psi = ShellOps.BuildOpenStartInfo(fullPath);

        // The attacker-influenced path must be exactly one argument, byte-for-byte —
        // never split into switches, never folded into the Arguments command line
        // where a '"' could break out and inject (the argument_injection finding).
        Assert.True(string.IsNullOrEmpty(psi.Arguments));
        Assert.Single(psi.ArgumentList);
        Assert.Equal(fullPath, psi.ArgumentList[0]);
        Assert.False(psi.UseShellExecute);
    }

    [Fact]
    public void BuildOpenStartInfo_LaunchesViaSystemExplorer()
    {
        var psi = ShellOps.BuildOpenStartInfo(@"C:\x");

        // De-elevation contract (CLAUDE.md UI invariants): targets open through
        // %WINDIR%\explorer.exe, pinned by full path against binary planting.
        Assert.EndsWith(@"\explorer.exe", psi.FileName, StringComparison.OrdinalIgnoreCase);
    }

    private sealed class RecordingRunner : IProcessRunner
    {
        internal ProcessStartInfo? Started { get; private set; }

        internal int Calls { get; private set; }

        public void Start(ProcessStartInfo psi)
        {
            Calls++;
            Started = psi;
        }
    }

    [Fact]
    public void OpenWith_drives_the_runner_with_the_path_as_one_verbatim_argument()
    {
        // "Open" used to call Process.Start directly, so nothing verified that the
        // built start info ever reached a launch. Drive a fake runner and assert it.
        var runner = new RecordingRunner();

        ShellOps.OpenWith(runner, "C:\\dir\\name with \" quote.txt");

        Assert.Equal(1, runner.Calls);
        Assert.NotNull(runner.Started);
        Assert.Single(runner.Started!.ArgumentList);
        Assert.Equal("C:\\dir\\name with \" quote.txt", runner.Started.ArgumentList[0]);
    }

    private sealed class RecordingExit : IAppExit
    {
        internal int Exits { get; private set; }

        public void Exit() => Exits++;
    }

    private sealed class ThrowingRunner : IProcessRunner
    {
        // Models a failed shell launch (e.g. the exe path vanished).
        public void Start(ProcessStartInfo psi) =>
            throw new System.ComponentModel.Win32Exception(2); // ERROR_FILE_NOT_FOUND
    }

    [Fact]
    public void RelaunchWith_starts_a_new_instance_then_requests_exit()
    {
        // Relaunch used to call Process.Start + Application.Current.Exit() inline,
        // so nothing verified the ordering or the marshaled exit. Drive both seams.
        var runner = new RecordingRunner();
        var exit = new RecordingExit();

        ShellOps.RelaunchWith(runner, exit);

        Assert.Equal(1, runner.Calls);
        Assert.NotNull(runner.Started);
        Assert.Equal(Environment.ProcessPath, runner.Started!.FileName);
        Assert.True(runner.Started!.UseShellExecute); // required for the shell relaunch
        Assert.Equal(1, exit.Exits); // exit is requested only after a successful launch
    }

    [Fact]
    public void RelaunchWith_does_not_exit_when_the_launch_fails()
    {
        // The invariant guarding the orphaned-window bug's evil twin: if the new
        // instance never started, never exit — that would kill the only window.
        var exit = new RecordingExit();

        ShellOps.RelaunchWith(new ThrowingRunner(), exit); // Run() swallows the failure

        Assert.Equal(0, exit.Exits);
    }
}
