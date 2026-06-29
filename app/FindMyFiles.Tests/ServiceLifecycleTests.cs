using System.Diagnostics;
using FindMyFiles.Services;
using Xunit;

namespace FindMyFiles.Tests;

/// <summary>
/// Defense-in-depth behavior tests for the service-lifecycle seams that the
/// thicker suites skipped: the simplest state transitions. Covers the
/// <see cref="ShellOps.RelaunchWith"/> argument plumbing that separates a plain
/// restart from the pipe-forcing relaunch (<c>--engine=pipe</c>), and the pure,
/// unelevated <see cref="ServiceSetup"/> surfaces (exe-location walk + SID
/// injection guard) that gate the one-time elevation. Anything touching the SCM,
/// elevation, or a real process launch is deliberately out of scope here.
/// </summary>
public sealed class ServiceLifecycleTests : IDisposable
{
    // RelaunchWith funnels failures through the process-wide Notifier (via Run);
    // reset it on teardown so a stray post can't replay into another test.
    public void Dispose()
    {
        Notifier.ResetForTests();
        GC.SuppressFinalize(this);
    }

    /// <summary>Captures the start info handed to the runner so the launch
    /// arguments (not just that a launch happened) can be asserted.</summary>
    private sealed class CapturingRunner : IProcessRunner
    {
        internal ProcessStartInfo? Started { get; private set; }

        public void Start(ProcessStartInfo psi) => Started = psi;
    }

    /// <summary>Minimal exit seam — counts requests without tearing anything
    /// down (the real exit is UI-thread-affine and out of scope here).</summary>
    private sealed class CountingExit : IAppExit
    {
        internal int Exits { get; private set; }

        public void Exit() => Exits++;
    }

    [Fact]
    public void Relaunch_with_pipe_argument_forces_the_pipe_transport_on_the_new_instance()
    {
        // The post-register relaunch must hand the fresh instance --engine=pipe so
        // it binds the retrying pipe client directly instead of re-running auto
        // detection (which can momentarily miss the warming service). Nothing
        // verified the flag actually reached the launch; this pins it verbatim.
        var runner = new CapturingRunner();

        ShellOps.RelaunchWith(runner, new CountingExit(), "--engine=pipe");

        Assert.NotNull(runner.Started);
        Assert.Equal("--engine=pipe", runner.Started!.Arguments);
    }

    [Fact]
    public void Relaunch_with_no_argument_starts_a_plain_instance_with_empty_command_line()
    {
        // The manual "restart app" path passes no argument, so settings.json's
        // auto transport stays authoritative — the new instance must launch with
        // an empty command line, never a leftover --engine=pipe.
        var runner = new CapturingRunner();

        ShellOps.RelaunchWith(runner, new CountingExit());

        Assert.NotNull(runner.Started);
        Assert.True(string.IsNullOrEmpty(runner.Started!.Arguments));
    }

    [Fact]
    public void Relaunch_with_pipe_argument_still_exits_only_after_a_successful_launch()
    {
        // The argument path must not change the ordering invariant: exit is
        // requested only once the new instance has actually started.
        var exit = new CountingExit();

        ShellOps.RelaunchWith(new CapturingRunner(), exit, "--engine=pipe");

        Assert.Equal(1, exit.Exits);
    }

    [Fact]
    public void Locate_service_exe_returns_null_when_nothing_is_present()
    {
        // The simplest transition: an isolated bin dir with no bundle and no dev
        // tree above it resolves to null, so the caller falls back to setup.
        var root = Directory.CreateTempSubdirectory("fmf-lifecycle-test");
        try
        {
            var baseDir = Path.Combine(root.FullName, "app", "bin");
            Directory.CreateDirectory(baseDir);

            Assert.Null(ServiceSetup.LocateServiceExe(baseDir));
        }
        finally
        {
            root.Delete(recursive: true);
        }
    }

    [Fact]
    public void Locate_service_exe_walks_up_several_levels_to_find_the_dev_tree()
    {
        // The dev-tree probe walks up from the bin dir; a deeply nested bin dir
        // must still find build\engine\release sitting near the repo root.
        var root = Directory.CreateTempSubdirectory("fmf-lifecycle-test");
        try
        {
            var baseDir = Path.Combine(root.FullName, "a", "b", "c", "bin");
            Directory.CreateDirectory(baseDir);

            var dev = Path.Combine(root.FullName, "build", "engine", "release");
            Directory.CreateDirectory(dev);
            var devExe = Path.Combine(dev, "fmf-service.exe");
            File.WriteAllText(devExe, string.Empty);

            Assert.Equal(devExe, ServiceSetup.LocateServiceExe(baseDir));
        }
        finally
        {
            root.Delete(recursive: true);
        }
    }

    [Fact]
    public void Locate_service_exe_returns_the_bundle_without_walking_when_present()
    {
        // A bundle next to the bin dir short-circuits the dev-tree walk entirely.
        var root = Directory.CreateTempSubdirectory("fmf-lifecycle-test");
        try
        {
            var baseDir = Path.Combine(root.FullName, "app", "bin");
            Directory.CreateDirectory(baseDir);
            var bundled = Path.Combine(baseDir, "fmf-service.exe");
            File.WriteAllText(bundled, string.Empty);

            Assert.Equal(bundled, ServiceSetup.LocateServiceExe(baseDir));
        }
        finally
        {
            root.Delete(recursive: true);
        }
    }

    [Theory]
    [InlineData("S-1-5")] // minimal well-formed prefix
    [InlineData("S-1-5-21-abc")] // letters allowed — it's an injection guard, not a strict parser
    public void Is_valid_sid_accepts_injection_safe_values(string input)
    {
        Assert.True(ServiceSetup.IsValidSid(input));
    }

    [Theory]
    [InlineData("s-1-5-18")] // lowercase prefix — the S-1- check is case-sensitive
    [InlineData("S-1-5-21-1\n--owner-sid=evil")] // newline injection
    [InlineData("S-1-5-21-1\t--owner-sid=evil")] // tab injection
    [InlineData("X-1-5-18")] // wrong authority prefix
    public void Is_valid_sid_rejects_malformed_or_injecting_values(string input)
    {
        Assert.False(ServiceSetup.IsValidSid(input));
    }
}
