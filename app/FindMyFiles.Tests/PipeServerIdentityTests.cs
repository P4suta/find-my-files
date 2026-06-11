using System.IO.Pipes;
using System.Security.Principal;
using FindMyFiles.Engine;
using FindMyFiles.Tests.TestDoubles;
using Xunit;

namespace FindMyFiles.Tests;

/// <summary>
/// Client-side fake-server defense (SECURITY.md 脅威4), unelevated: the test
/// process itself is the non-SYSTEM specimen, so the check must answer false
/// for it and for any server it hosts. Expectations are phrased against the
/// current identity so the suite also holds on a SYSTEM-run CI agent.
/// </summary>
public sealed class PipeServerIdentityTests
{
    private static bool RunningAsSystem
    {
        get
        {
            using var id = WindowsIdentity.GetCurrent();
            return id.IsSystem;
        }
    }

    [Fact]
    public void IsProcessSystem_OwnProcess_MatchesCurrentIdentity() =>
        Assert.Equal(
            RunningAsSystem,
            PipeServerIdentity.IsProcessSystem((uint)Environment.ProcessId));

    [Fact]
    public void IsProcessSystem_NonexistentPid_FailsClosed() =>
        // Real PIDs are multiples of 4 — 3 can never name a process, so
        // OpenProcess fails and the check must answer false, not throw.
        Assert.False(PipeServerIdentity.IsProcessSystem(3));

    [Fact]
    public async Task IsServerSystem_InProcessFakeServer_MatchesCurrentIdentity()
    {
        using var server = new FakePipeServer();
        using var stream = new NamedPipeClientStream(
            ".", server.PipeName, PipeDirection.InOut, PipeOptions.Asynchronous);
        await stream.ConnectAsync(new CancellationTokenSource(5000).Token);

        // The server end is this very test process — SYSTEM only if the
        // whole run is (normally it is not).
        Assert.Equal(RunningAsSystem, PipeServerIdentity.IsServerSystem(stream.SafePipeHandle));
    }
}
