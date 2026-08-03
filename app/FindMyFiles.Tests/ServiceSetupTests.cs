using System.Diagnostics;
using FindMyFiles.Engine;
using FindMyFiles.Services;
using FindMyFiles.Tests.TestDoubles;
using Xunit;

namespace FindMyFiles.Tests;

public sealed class ServiceSetupTests
{
    [Fact]
    public void Read_only_machine_probes_are_safe_without_a_registered_service()
    {
        _ = ServiceSetup.IsProcessElevated();
        Assert.True(Enum.IsDefined(ServiceSetup.QueryState()));
        _ = ServiceSetup.IsInstalledServiceCompatible();
        _ = ServiceSetup.QueryServiceProcessId();
    }

    [Fact]
    public void RunElevated_fails_closed_before_Uac_for_an_untrusted_helper()
    {
        var result = ServiceSetup.RunElevated("missing-fmf-service.exe", "setup");

        Assert.Equal(ServiceActionOutcome.Failed, result.Outcome);
        Assert.Equal(-1, result.ExitCode);
    }

    [Theory]
    [InlineData("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef", true)]
    [InlineData("0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF", false)]
    [InlineData("0123456789abcdef", false)]
    [InlineData("UNPINNED", false)]
    [InlineData(null, false)]
    public void ServiceImageDigest_requires_exact_lowercase_sha256(
        string? digest,
        bool expected) =>
        Assert.Equal(expected, ServiceExecutableTrust.IsPinnedDigest(digest));

    [Fact]
    public void Ordinary_test_build_cannot_cross_the_elevation_boundary()
    {
        Assert.Equal("UNPINNED", ServiceExecutableTrust.ExpectedImageSha256);
        Assert.Throws<System.Security.SecurityException>(
            () => ServiceExecutableTrust.Acquire("fmf-service.exe"));
    }

    [Fact]
    public void ServiceProtocolMarker_accepts_only_the_generated_exact_value()
    {
        Assert.True(
            ServiceSetup.IsServiceProtocolMarkerCompatible(
                EngineContract.ServiceProtocolMarker));
        Assert.False(ServiceSetup.IsServiceProtocolMarkerCompatible(null));
        Assert.False(ServiceSetup.IsServiceProtocolMarkerCompatible(string.Empty));
        Assert.False(
            ServiceSetup.IsServiceProtocolMarkerCompatible(
                EngineContract.ServiceProtocolMarker + " "));
        Assert.False(
            ServiceSetup.IsServiceProtocolMarkerCompatible(
                EngineContract.ServiceProtocolMarker.Replace(
                    $"protocol={EngineContract.ProtocolVersion}",
                    $"protocol={EngineContract.ProtocolVersion + 1}",
                    StringComparison.Ordinal)));
    }

    [Theory]
    [InlineData(1u, true)]
    [InlineData(2u, false)] // START_PENDING
    [InlineData(3u, false)] // STOP_PENDING
    [InlineData(4u, false)]
    [InlineData(5u, false)] // CONTINUE_PENDING
    [InlineData(6u, false)] // PAUSE_PENDING
    [InlineData(7u, false)] // PAUSED
    [InlineData(0u, false)] // malformed: fail closed
    public void MapServiceState_only_treats_SERVICE_STOPPED_as_stopped(
        uint raw,
        bool expectedStopped) =>
        Assert.Equal(
            expectedStopped ? EngineServiceState.Stopped : EngineServiceState.Running,
            ServiceSetup.MapServiceState(raw));

    [Fact]
    public void DirectScmStart_IsIdempotentWhenAlreadyRunning()
    {
        var starts = 0;
        var result = ServiceSetup.DriveServiceControl(
            ServiceSetup.ScmControlVerb.Start,
            () => 4,
            () =>
            {
                starts++;
                return 0;
            },
            () => throw new InvalidOperationException(),
            maxPollAttempts: 2,
            () => throw new InvalidOperationException());

        Assert.True(result);
        Assert.Equal(0, starts);
    }

    [Fact]
    public void DirectScmStart_ToleratesAnotherCallerWinningTheRace()
    {
        var states = new Queue<uint?>([1, 2, 4]);
        var starts = 0;
        var waits = 0;
        var result = ServiceSetup.DriveServiceControl(
            ServiceSetup.ScmControlVerb.Start,
            () => states.Dequeue(),
            () =>
            {
                starts++;
                return 1056; // ERROR_SERVICE_ALREADY_RUNNING
            },
            () => throw new InvalidOperationException(),
            maxPollAttempts: 3,
            () => waits++);

        Assert.True(result);
        Assert.Equal(1, starts);
        Assert.Equal(2, waits);
    }

    [Fact]
    public void DirectScmStop_ToleratesAnotherCallerWinningTheRace()
    {
        var states = new Queue<uint?>([4, 3, 1]);
        var stops = 0;
        var result = ServiceSetup.DriveServiceControl(
            ServiceSetup.ScmControlVerb.Stop,
            () => states.Dequeue(),
            () => throw new InvalidOperationException(),
            () =>
            {
                stops++;
                return 1062; // ERROR_SERVICE_NOT_ACTIVE
            },
            maxPollAttempts: 3,
            () => { });

        Assert.True(result);
        Assert.Equal(1, stops);
    }

    [Fact]
    public void DirectScmRestart_UsesOneBoundedStopThenStartSequence()
    {
        var states = new Queue<uint?>([4, 3, 1, 1, 2, 4]);
        var starts = 0;
        var stops = 0;
        var waits = 0;
        var result = ServiceSetup.DriveServiceControl(
            ServiceSetup.ScmControlVerb.Restart,
            () => states.Dequeue(),
            () =>
            {
                starts++;
                return 0;
            },
            () =>
            {
                stops++;
                return 0;
            },
            maxPollAttempts: 6,
            () => waits++);

        Assert.True(result);
        Assert.Equal(1, stops);
        Assert.Equal(1, starts);
        Assert.Equal(4, waits);
        Assert.Empty(states);
    }

    [Fact]
    public void DirectScmControl_TimesOutWithinTheSharedPollBudget()
    {
        var queries = 0;
        var waits = 0;
        var result = ServiceSetup.DriveServiceControl(
            ServiceSetup.ScmControlVerb.Start,
            () =>
            {
                queries++;
                return 2; // SERVICE_START_PENDING forever
            },
            () => throw new InvalidOperationException(),
            () => throw new InvalidOperationException(),
            maxPollAttempts: 3,
            () => waits++);

        Assert.False(result);
        Assert.Equal(3, queries);
        Assert.Equal(2, waits);
    }

    [Fact]
    public void DirectScmControl_FailsClosedOnAccessOrQueryFailure()
    {
        Assert.False(ServiceSetup.DriveServiceControl(
            ServiceSetup.ScmControlVerb.Start,
            () => 1,
            () => 5, // ERROR_ACCESS_DENIED
            () => 0,
            maxPollAttempts: 2,
            () => { }));
        Assert.False(ServiceSetup.DriveServiceControl(
            ServiceSetup.ScmControlVerb.Stop,
            () => null,
            () => 0,
            () => 0,
            maxPollAttempts: 2,
            () => { }));
    }

    [Fact]
    public void DirectScmControl_rejects_invalid_and_terminal_failure_states()
    {
        Assert.False(ServiceSetup.DriveServiceControl(
            (ServiceSetup.ScmControlVerb)99,
            () => throw new InvalidOperationException(),
            () => throw new InvalidOperationException(),
            () => throw new InvalidOperationException(),
            maxPollAttempts: 1,
            () => throw new InvalidOperationException()));

        Assert.False(ServiceSetup.DriveServiceControl(
            ServiceSetup.ScmControlVerb.Start,
            new Queue<uint?>([1, 1]).Dequeue,
            () => 0,
            () => throw new InvalidOperationException(),
            maxPollAttempts: 2,
            () => { }));
        Assert.False(ServiceSetup.DriveServiceControl(
            ServiceSetup.ScmControlVerb.Start,
            () => 1,
            () => 5,
            () => throw new InvalidOperationException(),
            maxPollAttempts: 2,
            () => { }));
        Assert.False(ServiceSetup.DriveServiceControl(
            ServiceSetup.ScmControlVerb.Start,
            () => 7,
            () => throw new InvalidOperationException(),
            () => throw new InvalidOperationException(),
            maxPollAttempts: 1,
            () => { }));

        Assert.True(ServiceSetup.DriveServiceControl(
            ServiceSetup.ScmControlVerb.Stop,
            () => 1,
            () => throw new InvalidOperationException(),
            () => throw new InvalidOperationException(),
            maxPollAttempts: 1,
            () => throw new InvalidOperationException()));
        Assert.False(ServiceSetup.DriveServiceControl(
            ServiceSetup.ScmControlVerb.Stop,
            () => 4,
            () => throw new InvalidOperationException(),
            () => 5,
            maxPollAttempts: 2,
            () => { }));
        Assert.False(ServiceSetup.DriveServiceControl(
            ServiceSetup.ScmControlVerb.Stop,
            () => 0,
            () => throw new InvalidOperationException(),
            () => throw new InvalidOperationException(),
            maxPollAttempts: 1,
            () => { }));
    }

    [Fact]
    public void DirectScmStop_retries_a_cannot_accept_control_race()
    {
        var states = new Queue<uint?>([4, 4, 1]);
        var stops = new Queue<int>([1061, 0]);
        var waits = 0;

        Assert.True(ServiceSetup.DriveServiceControl(
            ServiceSetup.ScmControlVerb.Stop,
            states.Dequeue,
            () => throw new InvalidOperationException(),
            stops.Dequeue,
            maxPollAttempts: 3,
            () => waits++));
        Assert.Equal(2, waits);
        Assert.Empty(states);
        Assert.Empty(stops);
    }

    [Fact]
    public void DirectScmControl_rejects_a_nonpositive_poll_budget()
    {
        Assert.Throws<ArgumentOutOfRangeException>(() => ServiceSetup.DriveServiceControl(
            ServiceSetup.ScmControlVerb.Start,
            () => 1,
            () => 0,
            () => 0,
            maxPollAttempts: 0,
            () => { }));
    }

    [Fact]
    public void LocateServiceExe_PrefersBundled_ThenDevTree_ElseNull()
    {
        var root = Directory.CreateTempSubdirectory("fmf-setup-test");
        try
        {
            var baseDir = Path.Combine(root.FullName, "app", "bin");
            Directory.CreateDirectory(baseDir);
            Assert.Null(ServiceSetup.LocateServiceExe(baseDir));

            // Dev tree: build\engine\release above the bin dir.
            var dev = Path.Combine(root.FullName, "build", "engine", "release");
            Directory.CreateDirectory(dev);
            var devExe = Path.Combine(dev, "fmf-service.exe");
            File.WriteAllText(devExe, string.Empty);
            Assert.Equal(devExe, ServiceSetup.LocateServiceExe(baseDir));

            // The dist bundle wins over the dev tree.
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
    [InlineData("S-1-5-21-1654600493-3733564142-2704359447-1001", true)]
    [InlineData("S-1-5-18", true)] // well-formed (validate_user_sid rejects it server-side)
    [InlineData(null, false)]
    [InlineData("", false)]
    [InlineData("not-a-sid", false)]
    [InlineData("S-1-5-21-abc", false)]
    [InlineData("S-1-05-18", false)]
    [InlineData("S-2-5-18", false)]
    [InlineData("S-1-281474976710656-18", false)]
    [InlineData("S-1-5-4294967296", false)]
    [InlineData("S-1-5-21-1; rm -rf", false)] // ; and space — injection attempt
    [InlineData("S-1-5-21-1 --owner-sid=evil", false)] // space would split into args
    [InlineData("S-1-5-21-１", false)] // full-width digit is not ASCII
    [InlineData("S-1-5-+1", false)] // uint accepts a sign; canonical SID syntax does not
    public void IsValidSid_AcceptsWellFormed_RejectsInjection(string? input, bool expected)
    {
        Assert.Equal(expected, ServiceSetup.IsValidSid(input));
    }

    [Fact]
    public void CurrentUserSid_ReturnsForwardableSid()
    {
        var sid = ServiceSetup.CurrentUserSid();
        Assert.NotNull(sid);
        Assert.StartsWith("S-1-", sid, StringComparison.Ordinal);
        Assert.True(ServiceSetup.IsValidSid(sid), "own SID must survive the injection guard");
    }

    [Fact]
    public void TryCreateSetupArguments_BindsTheExactValidatedOwner()
    {
        const string sid = "S-1-5-21-1654600493-3733564142-2704359447-1001";

        Assert.True(ServiceSetup.TryCreateSetupArguments(sid, out var arguments));
        Assert.Equal($"setup --owner-sid={sid}", arguments);
    }

    [Theory]
    [InlineData(null)]
    [InlineData("")]
    [InlineData("not-a-sid")]
    [InlineData("S-1-5-21-1 --owner-sid=S-1-1-0")]
    public void TryCreateSetupArguments_HasNoOwnerlessFallback(string? sid)
    {
        Assert.False(ServiceSetup.TryCreateSetupArguments(sid, out var arguments));
        Assert.Empty(arguments);
    }

    [Fact]
    public void PollForCompatibleStartedService_WaitsForRunningThenCurrentPipe()
    {
        var pids = new Queue<uint>([0, 0, 42]);
        var probes = new Queue<bool>([false, true]);
        var waits = 0;

        var compatible = ServiceSetup.PollForCompatibleStartedService(
            () => pids.Dequeue(),
            () => probes.Dequeue(),
            startPollAttempts: 3,
            compatibilityProbeAttempts: 2,
            () => waits++);

        Assert.True(compatible);
        Assert.Equal(3, waits); // two START_PENDING polls + one probe grace
        Assert.Empty(pids);
        Assert.Empty(probes);
    }

    [Fact]
    public void PollForCompatibleStartedService_RejectsObsoletePipe()
    {
        var probes = 0;
        var compatible = ServiceSetup.PollForCompatibleStartedService(
            () => 42,
            () =>
            {
                probes++;
                return false;
            },
            startPollAttempts: 10,
            compatibilityProbeAttempts: 3,
            () => { });

        Assert.False(compatible);
        Assert.Equal(3, probes);
    }

    [Fact]
    public void PollForCompatibleStartedService_DoesNotProbeBeforeRunning()
    {
        var probes = 0;
        var compatible = ServiceSetup.PollForCompatibleStartedService(
            () => 0,
            () =>
            {
                probes++;
                return true;
            },
            startPollAttempts: 3,
            compatibilityProbeAttempts: 2,
            () => { });

        Assert.False(compatible);
        Assert.Equal(0, probes);
    }

    [Fact]
    public void PollForCompatibleStartedService_can_succeed_without_waiting()
    {
        Assert.True(ServiceSetup.PollForCompatibleStartedService(
            () => 42,
            () => true,
            startPollAttempts: 1,
            compatibilityProbeAttempts: 1,
            () => throw new InvalidOperationException()));
    }

    [Theory]
    [InlineData(0, 1)]
    [InlineData(1, 0)]
    public void PollForCompatibleStartedService_rejects_nonpositive_budgets(
        int startAttempts,
        int probeAttempts)
    {
        Assert.Throws<ArgumentOutOfRangeException>(() =>
            ServiceSetup.PollForCompatibleStartedService(
                () => 0,
                () => false,
                startAttempts,
                probeAttempts,
                () => { }));
    }

    [Fact]
    public void Native_query_policies_fail_closed_when_handles_are_unavailable()
    {
        var harness = new ServiceSetupHarness { Manager = null };
        using var scope = ServiceSetup.UseHooksForTests(harness.Hooks);

        Assert.Equal(EngineServiceState.Unknown, ServiceSetup.QueryState());
        Assert.False(ServiceSetup.IsInstalledServiceCompatible());
        Assert.Equal(0u, ServiceSetup.QueryServiceProcessId());

        harness.Manager = new FakeServiceManager { LastError = 1060 };
        Assert.Equal(EngineServiceState.NotInstalled, ServiceSetup.QueryState());
        Assert.False(ServiceSetup.IsInstalledServiceCompatible());
        Assert.Equal(0u, ServiceSetup.QueryServiceProcessId());

        harness.Manager.LastError = 5;
        Assert.Equal(EngineServiceState.Unknown, ServiceSetup.QueryState());
    }

    [Fact]
    public void Native_query_policies_map_status_description_and_process_results()
    {
        var service = new FakeServiceHandle();
        var harness = new ServiceSetupHarness
        {
            Manager = new FakeServiceManager { Service = service },
        };
        using var scope = ServiceSetup.UseHooksForTests(harness.Hooks);

        service.QueryStateSucceeds = false;
        Assert.Equal(EngineServiceState.Unknown, ServiceSetup.QueryState());
        service.QueryStateSucceeds = true;
        service.State = 1;
        Assert.Equal(EngineServiceState.Stopped, ServiceSetup.QueryState());
        service.State = 4;
        Assert.Equal(EngineServiceState.Running, ServiceSetup.QueryState());

        service.DescriptionBytesNeeded = 0;
        Assert.False(ServiceSetup.IsInstalledServiceCompatible());
        Assert.Equal(0, service.DescriptionReadCalls);
        service.DescriptionBytesNeeded = 4097;
        Assert.False(ServiceSetup.IsInstalledServiceCompatible());
        Assert.Equal(0, service.DescriptionReadCalls);
        service.DescriptionBytesNeeded = 64;
        service.ReadDescriptionSucceeds = false;
        Assert.False(ServiceSetup.IsInstalledServiceCompatible());
        service.ReadDescriptionSucceeds = true;
        service.Description = "obsolete";
        Assert.False(ServiceSetup.IsInstalledServiceCompatible());
        service.Description = EngineContract.ServiceProtocolMarker;
        Assert.True(ServiceSetup.IsInstalledServiceCompatible());

        service.QueryProcessSucceeds = false;
        Assert.Equal(0u, ServiceSetup.QueryServiceProcessId());
        service.QueryProcessSucceeds = true;
        service.State = 1;
        service.ProcessId = 42;
        Assert.Equal(0u, ServiceSetup.QueryServiceProcessId());
        service.State = 4;
        Assert.Equal(42u, ServiceSetup.QueryServiceProcessId());
    }

    [Theory]
    [InlineData(0, true)]
    [InlineData(5, false)]
    public void RunElevated_classifies_exit_and_preserves_closed_start_info(
        int exitCode,
        bool expectedSuccess)
    {
        var process = new FakeElevatedProcess { ExitCodeValue = exitCode };
        var harness = new ServiceSetupHarness { Process = process };
        using var scope = ServiceSetup.UseHooksForTests(harness.Hooks);

        var result = ServiceSetup.RunElevated("helper.exe", "setup --owner-sid=S-1-5-18");

        Assert.Equal(
            expectedSuccess ? ServiceActionOutcome.Ok : ServiceActionOutcome.Failed,
            result.Outcome);
        Assert.Equal(exitCode, result.ExitCode);
        Assert.Equal("helper.exe", harness.AcquiredPath);
        Assert.NotNull(harness.StartInfo);
        Assert.Equal("helper.exe", harness.StartInfo.FileName);
        Assert.Equal("setup --owner-sid=S-1-5-18", harness.StartInfo.Arguments);
        Assert.True(harness.StartInfo.UseShellExecute);
        Assert.Equal("runas", harness.StartInfo.Verb);
        Assert.Equal(ProcessWindowStyle.Hidden, harness.StartInfo.WindowStyle);
        Assert.True(harness.LeaseDisposed);
        Assert.True(process.Disposed);
    }

    [Fact]
    public void RunElevated_covers_missing_cancelled_failed_and_timed_out_processes()
    {
        var harness = new ServiceSetupHarness { Process = null };
        using var scope = ServiceSetup.UseHooksForTests(harness.Hooks);
        Assert.Equal(
            new ServiceActionResult(ServiceActionOutcome.Failed, -1),
            ServiceSetup.RunElevated("a", "b"));

        harness.StartException = new System.ComponentModel.Win32Exception(1223);
        Assert.Equal(
            new ServiceActionResult(ServiceActionOutcome.Cancelled, -1),
            ServiceSetup.RunElevated("a", "b"));

        harness.StartException = new InvalidOperationException("boom");
        Assert.Equal(ServiceActionOutcome.Failed, ServiceSetup.RunElevated("a", "b").Outcome);

        harness.StartException = null;
        harness.Process = new FakeElevatedProcess { WaitResult = false };
        Assert.Equal(
            new ServiceActionResult(ServiceActionOutcome.Failed, -1),
            ServiceSetup.RunElevated("a", "b"));
        Assert.True(harness.Process.Killed);
        Assert.Equal(2, harness.Process.WaitCalls);

        harness.Process = new FakeElevatedProcess
        {
            WaitResult = false,
            KillException = new InvalidOperationException("deny"),
        };
        Assert.Equal(ServiceActionOutcome.Failed, ServiceSetup.RunElevated("a", "b").Outcome);

        harness.AcquireException = new InvalidOperationException("untrusted");
        Assert.Equal(ServiceActionOutcome.Failed, ServiceSetup.RunElevated("a", "b").Outcome);
    }

    [Fact]
    public void Unelevated_control_uses_minimal_access_and_native_results()
    {
        var service = new FakeServiceHandle();
        var manager = new FakeServiceManager { Service = service };
        var harness = new ServiceSetupHarness { Manager = manager };
        using var scope = ServiceSetup.UseHooksForTests(harness.Hooks);

        service.States = new Queue<uint>([1, 4]);
        Assert.True(ServiceSetup.TryStartUnelevated());
        Assert.Equal(0x0014u, manager.LastAccess);
        Assert.Equal(1, service.StartCalls);

        service.States = new Queue<uint>([4, 1]);
        Assert.True(ServiceSetup.TryStopUnelevated());
        Assert.Equal(0x0024u, manager.LastAccess);
        Assert.Equal(1, service.StopCalls);

        service.States = new Queue<uint>([4, 1, 1, 4]);
        Assert.True(ServiceSetup.TryRestartUnelevated());
        Assert.Equal(0x0034u, manager.LastAccess);
        Assert.Equal(2, service.StartCalls);
        Assert.Equal(2, service.StopCalls);
        Assert.Equal(4, harness.WaitCalls);
    }

    [Fact]
    public void Unelevated_control_contains_manager_service_and_query_failures()
    {
        var harness = new ServiceSetupHarness { Manager = null };
        using var scope = ServiceSetup.UseHooksForTests(harness.Hooks);
        Assert.False(ServiceSetup.TryStartUnelevated());

        harness.Manager = new FakeServiceManager { LastError = 5 };
        Assert.False(ServiceSetup.TryStopUnelevated());

        harness.Manager.Service = new FakeServiceHandle { QueryStateSucceeds = false };
        Assert.False(ServiceSetup.TryRestartUnelevated());

        harness.OpenManagerException = new InvalidOperationException("SCM unavailable");
        Assert.False(ServiceSetup.TryStartUnelevated());
    }

    [Fact]
    public void Wait_and_identity_public_boundaries_use_one_hook_snapshot()
    {
        var service = new FakeServiceHandle
        {
            ProcessResults = new Queue<(bool, uint, uint)>(
            [
                (true, 2, 0),
                (true, 4, 42),
            ]),
        };
        var harness = new ServiceSetupHarness
        {
            Manager = new FakeServiceManager { Service = service },
            ProbeResults = new Queue<bool>([false, true]),
            Sid = "S-1-5-18",
        };
        using var scope = ServiceSetup.UseHooksForTests(harness.Hooks);

        Assert.True(ServiceSetup.WaitForCompatibleStartedService("current-pipe"));
        Assert.Equal("current-pipe", harness.LastPipeName);
        Assert.Equal(2, harness.WaitCalls);
        Assert.Equal("S-1-5-18", ServiceSetup.CurrentUserSid());
        Assert.True(ServiceSetup.TryCreateSetupArguments(out var arguments));
        Assert.Equal("setup --owner-sid=S-1-5-18", arguments);

        harness.SidException = new InvalidOperationException("token unavailable");
        Assert.Null(ServiceSetup.CurrentUserSid());
        Assert.False(ServiceSetup.TryCreateSetupArguments(out arguments));
        Assert.Empty(arguments);
    }

    [Fact]
    public void Hook_scope_rejects_null_and_restores_the_previous_set()
    {
        Assert.Throws<ArgumentNullException>(
            () => ServiceSetup.UseHooksForTests(null!));

        var outer = new ServiceSetupHarness { Manager = null };
        var inner = new ServiceSetupHarness
        {
            Manager = new FakeServiceManager
            {
                Service = new FakeServiceHandle
                {
                    QueryStateSucceeds = true,
                    State = 1,
                },
            },
        };
        using var outerScope = ServiceSetup.UseHooksForTests(outer.Hooks);
        Assert.Equal(EngineServiceState.Unknown, ServiceSetup.QueryState());
        var innerScope = ServiceSetup.UseHooksForTests(inner.Hooks);
        Assert.Equal(EngineServiceState.Stopped, ServiceSetup.QueryState());
        innerScope.Dispose();

        Assert.Equal(EngineServiceState.Unknown, ServiceSetup.QueryState());

        var current = new ServiceSetupHarness
        {
            Manager = new FakeServiceManager
            {
                Service = new FakeServiceHandle
                {
                    QueryStateSucceeds = true,
                    State = 4,
                },
            },
        };
        using var currentScope = ServiceSetup.UseHooksForTests(current.Hooks);
        innerScope.Dispose();
        Assert.Equal(EngineServiceState.Running, ServiceSetup.QueryState());
    }

    [Fact]
    public void Direct_control_covers_invalid_verb_and_exhausted_shared_budgets()
    {
        var service = new FakeServiceHandle { State = 1 };
        var harness = new ServiceSetupHarness
        {
            Manager = new FakeServiceManager { Service = service },
        };
        using var scope = ServiceSetup.UseHooksForTests(harness.Hooks);

        Assert.False(ServiceSetup.TryControlUnelevated((ServiceSetup.ScmControlVerb)99));
        Assert.False(ServiceSetup.DriveServiceControl(
            ServiceSetup.ScmControlVerb.Start,
            () => null,
            () => 0,
            () => 0,
            maxPollAttempts: 1,
            () => { }));
        Assert.False(ServiceSetup.DriveServiceControl(
            ServiceSetup.ScmControlVerb.Stop,
            () => 4,
            () => 0,
            () => 0,
            maxPollAttempts: 1,
            () => { }));

        var states = new Queue<uint?>([4, 1]);
        Assert.False(ServiceSetup.DriveServiceControl(
            ServiceSetup.ScmControlVerb.Restart,
            states.Dequeue,
            () => 0,
            () => 0,
            maxPollAttempts: 2,
            () => { }));
    }

    [Fact]
    public void Description_and_sid_accept_their_exact_maximum_boundaries()
    {
        var service = new FakeServiceHandle
        {
            DescriptionBytesNeeded = 4096,
            Description = EngineContract.ServiceProtocolMarker,
        };
        var harness = new ServiceSetupHarness
        {
            Manager = new FakeServiceManager { Service = service },
        };
        using var scope = ServiceSetup.UseHooksForTests(harness.Hooks);

        Assert.True(ServiceSetup.IsInstalledServiceCompatible());

        var maximum = "S-1-281474976710655"
            + string.Concat(Enumerable.Repeat("-4294967295", 15));
        Assert.Equal(184, maximum.Length);
        Assert.True(ServiceSetup.IsValidSid(maximum));
        Assert.False(ServiceSetup.IsValidSid(maximum + "-1"));
        Assert.False(ServiceSetup.IsValidSid("S-1-5-"));
        Assert.False(ServiceSetup.IsValidSid("S-1-5-1x"));
    }

    [Theory]
    [InlineData(false)]
    [InlineData(true)]
    public void Process_elevation_is_read_through_the_atomic_boundary(bool elevated)
    {
        var harness = new ServiceSetupHarness { ProcessElevated = elevated };
        using var scope = ServiceSetup.UseHooksForTests(harness.Hooks);

        Assert.Equal(elevated, ServiceSetup.IsProcessElevated());
    }

    [Fact]
    public void LocateServiceExe_never_walks_beyond_eight_ancestors()
    {
        var root = Directory.CreateTempSubdirectory("fmf-setup-bounded-");
        try
        {
            var baseDir = root.FullName;
            for (var depth = 0; depth < 8; depth++)
            {
                baseDir = Path.Combine(
                    baseDir,
                    depth.ToString(System.Globalization.CultureInfo.InvariantCulture));
            }

            Directory.CreateDirectory(baseDir);
            var tooDistant = Path.Combine(root.FullName, "build", "engine", "release");
            Directory.CreateDirectory(tooDistant);
            File.WriteAllText(Path.Combine(tooDistant, "fmf-service.exe"), string.Empty);

            Assert.Null(ServiceSetup.LocateServiceExe(baseDir));
        }
        finally
        {
            root.Delete(recursive: true);
        }
    }

    [Fact]
    public void Direct_control_issues_each_accepted_start_or_stop_only_once()
    {
        var starts = 0;
        Assert.False(ServiceSetup.DriveServiceControl(
            ServiceSetup.ScmControlVerb.Start,
            new Queue<uint?>([1, 1]).Dequeue,
            () =>
            {
                starts++;
                return 0;
            },
            () => 0,
            maxPollAttempts: 2,
            () => { }));
        Assert.Equal(1, starts);

        var stops = 0;
        Assert.True(ServiceSetup.DriveServiceControl(
            ServiceSetup.ScmControlVerb.Stop,
            new Queue<uint?>([4, 4, 1]).Dequeue,
            () => 0,
            () =>
            {
                stops++;
                return 0;
            },
            maxPollAttempts: 3,
            () => { }));
        Assert.Equal(1, stops);
    }

    [Fact]
    public void Service_setup_failures_and_control_results_are_structurally_logged()
    {
        using var log = new LogCapture();
        var harness = new ServiceSetupHarness
        {
            AcquireException = new InvalidOperationException("untrusted"),
            Manager = null,
            SidException = new InvalidOperationException("token"),
        };
        using var scope = ServiceSetup.UseHooksForTests(harness.Hooks);

        Assert.Equal(ServiceActionOutcome.Failed, ServiceSetup.RunElevated("a", "b").Outcome);
        Assert.False(ServiceSetup.TryStartUnelevated());
        _ = ServiceSetup.CurrentUserSid();

        harness.OpenManagerException = new InvalidOperationException("SCM");
        Assert.False(ServiceSetup.TryRestartUnelevated());
        harness.OpenManagerException = null;

        harness.Manager = new FakeServiceManager { LastError = 5 };
        Assert.False(ServiceSetup.TryStopUnelevated());

        var service = new FakeServiceHandle { State = 4 };
        harness.Manager.Service = service;
        Assert.True(ServiceSetup.TryStartUnelevated());

        harness.Process = new FakeElevatedProcess { WaitResult = false };
        harness.AcquireException = null;
        _ = ServiceSetup.RunElevated("helper", "setup");
        harness.Process = new FakeElevatedProcess
        {
            WaitResult = false,
            KillException = new InvalidOperationException("deny"),
        };
        _ = ServiceSetup.RunElevated("helper", "setup");

        var lines = log.Text.Split('\n');
        AssertLog(lines, "area=service-setup", "elevated service action failed");
        AssertLog(
            lines,
            "area=service-setup",
            "verb=Start",
            "win32=-1",
            "could not open SCM for unelevated service control");
        AssertLog(
            lines,
            "area=service-setup",
            "verb=Stop",
            "win32=5",
            "could not open service for unelevated control");
        AssertLog(
            lines,
            "area=service-setup",
            "verb=Start",
            "success=true",
            "unelevated SCM control completed");
        AssertLog(
            lines,
            "area=service-setup",
            "mode=elevated",
            "pid=42",
            "timed-out service helper terminated");
        AssertLog(
            lines,
            "area=service-setup",
            "could not terminate timed-out elevated service helper");
        AssertLog(
            lines,
            "area=service-setup",
            "unelevated SCM Restart failed");
        AssertLog(
            lines,
            "area=service-setup",
            "current user SID query failed");
    }

    private static void AssertLog(string[] lines, params string[] fragments) =>
        Assert.Contains(
            lines,
            line => fragments.All(fragment => line.Contains(fragment, StringComparison.Ordinal)));

    private sealed class ServiceSetupHarness
    {
        public FakeServiceManager? Manager { get; set; }

        public Exception? OpenManagerException { get; set; }

        public Exception? AcquireException { get; set; }

        public Exception? StartException { get; set; }

        public Exception? SidException { get; set; }

        public FakeElevatedProcess? Process { get; set; }

        public bool ProcessElevated { get; set; }

        public string? Sid { get; set; }

        public string? AcquiredPath { get; private set; }

        public ProcessStartInfo? StartInfo { get; private set; }

        public Queue<bool> ProbeResults { get; set; } = new([true]);

        public string? LastPipeName { get; private set; }

        public int WaitCalls { get; private set; }

        public bool LeaseDisposed { get; private set; }

        public ServiceSetupHooks Hooks => new(
            _ =>
            {
                if (OpenManagerException is not null)
                {
                    throw OpenManagerException;
                }

                return Manager;
            },
            path =>
            {
                if (AcquireException is not null)
                {
                    throw AcquireException;
                }

                AcquiredPath = path;
                return new TrustedServiceExecutable(
                    path,
                    new CallbackDisposable(() => LeaseDisposed = true));
            },
            info =>
            {
                if (StartException is not null)
                {
                    throw StartException;
                }

                StartInfo = info;
                return Process;
            },
            () => ProcessElevated,
            () =>
            {
                if (SidException is not null)
                {
                    throw SidException;
                }

                return Sid;
            },
            pipeName =>
            {
                LastPipeName = pipeName;
                return ProbeResults.Dequeue();
            },
            _ => WaitCalls++);
    }

    private sealed class FakeServiceManager : IServiceManagerHandle
    {
        public int LastError { get; set; }

        public FakeServiceHandle? Service { get; set; }

        public uint LastAccess { get; private set; }

        public IServiceHandle? OpenService(string name, uint access)
        {
            Assert.Equal(EngineContract.ServiceName, name);
            LastAccess = access;
            return Service;
        }

        public void Dispose()
        {
        }
    }

    private sealed class FakeServiceHandle : IServiceHandle
    {
        public bool QueryStateSucceeds { get; set; } = true;

        public uint State { get; set; }

        public Queue<uint>? States { get; set; }

        public uint DescriptionBytesNeeded { get; set; } = 64;

        public bool ReadDescriptionSucceeds { get; set; } = true;

        public int DescriptionReadCalls { get; private set; }

        public string? Description { get; set; }

        public bool QueryProcessSucceeds { get; set; } = true;

        public uint ProcessId { get; set; }

        public Queue<(bool Success, uint State, uint ProcessId)>? ProcessResults { get; set; }

        public int StartResult { get; set; }

        public int StopResult { get; set; }

        public int StartCalls { get; private set; }

        public int StopCalls { get; private set; }

        public bool TryQueryState(out uint state)
        {
            state = States?.Dequeue() ?? State;
            return QueryStateSucceeds;
        }

        public uint QueryDescriptionBytesNeeded() => DescriptionBytesNeeded;

        public bool TryReadDescription(uint bytesNeeded, out string? description)
        {
            DescriptionReadCalls++;
            Assert.Equal(DescriptionBytesNeeded, bytesNeeded);
            description = Description;
            return ReadDescriptionSucceeds;
        }

        public bool TryQueryProcess(out uint state, out uint processId)
        {
            if (ProcessResults is not null)
            {
                var result = ProcessResults.Dequeue();
                state = result.State;
                processId = result.ProcessId;
                return result.Success;
            }

            state = State;
            processId = ProcessId;
            return QueryProcessSucceeds;
        }

        public int Start()
        {
            StartCalls++;
            return StartResult;
        }

        public int Stop()
        {
            StopCalls++;
            return StopResult;
        }

        public void Dispose()
        {
        }
    }

    private sealed class FakeElevatedProcess : IElevatedProcess
    {
        public int ExitCodeValue { get; set; }

        public bool WaitResult { get; set; } = true;

        public Exception? KillException { get; set; }

        public bool Killed { get; private set; }

        public bool Disposed { get; private set; }

        public int WaitCalls { get; private set; }

        public int ExitCode => ExitCodeValue;

        public int Id => 42;

        public bool WaitForExit(int milliseconds)
        {
            WaitCalls++;
            return WaitResult;
        }

        public void Kill(bool entireProcessTree)
        {
            Killed = entireProcessTree;
            if (KillException is not null)
            {
                throw KillException;
            }
        }

        public void Dispose() => Disposed = true;
    }

    private sealed class CallbackDisposable(Action dispose) : IDisposable
    {
        public void Dispose() => dispose();
    }
}
