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

    private sealed class RecordingRestart : IAppRestart
    {
        internal int Calls { get; private set; }

        internal string? LastArguments { get; private set; }

        public void Restart(string arguments)
        {
            Calls++;
            LastArguments = arguments;
        }
    }

    private sealed class ThrowingRestart : IAppRestart
    {
        // Models a failed restart: AppInstance.Restart returns a failure reason,
        // which RealAppRestart surfaces as an exception for ShellOps.Run to catch.
        public void Restart(string arguments) =>
            throw new InvalidOperationException("restart failed");
    }

    [Fact]
    public void RelaunchWith_restarts_the_app_with_empty_arguments()
    {
        // The language switch is the only true restart left (ADR-0036): it must hand
        // the fresh instance an empty command line, so settings.json's saved language
        // drives the new instance rather than a stale --engine override.
        var restart = new RecordingRestart();

        ShellOps.RelaunchWith(restart);

        Assert.Equal(1, restart.Calls);
        Assert.Equal(string.Empty, restart.LastArguments);
    }

    [Fact]
    public void RelaunchWith_swallows_a_failed_restart_instead_of_throwing()
    {
        // A restart failure is funneled through ShellOps.Run (notify, don't crash):
        // the call must return normally rather than propagate — Dispose resets the
        // Notifier the swallowed failure posted to.
        ShellOps.RelaunchWith(new ThrowingRestart());
    }
}
