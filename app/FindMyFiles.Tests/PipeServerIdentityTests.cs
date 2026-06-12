using System.IO.Pipes;
using FindMyFiles.Engine;
using FindMyFiles.Services;
using FindMyFiles.Tests.TestDoubles;
using Xunit;

namespace FindMyFiles.Tests;

/// <summary>
/// Client-side fake-server defense (SECURITY.md 脅威4): a pipe is trusted only
/// when its server process is the SCM-registered fmf-engine service. The test
/// process is never that service, so the check answers false for it and for
/// any in-process server it hosts. Works unelevated (no SYSTEM token reads).
/// </summary>
public sealed class PipeServerIdentityTests
{
    [Fact]
    public void IsServiceProcess_OwnProcess_IsFalse() =>
        Assert.False(PipeServerIdentity.IsServiceProcess((uint)Environment.ProcessId));

    [Fact]
    public void IsServiceProcess_NonexistentPid_FailsClosed() =>
        // Real PIDs are multiples of 4 — 3 can never name a process, so the
        // check must answer false, not throw.
        Assert.False(PipeServerIdentity.IsServiceProcess(3));

    [Fact]
    public void IsServiceProcess_MatchesQueriedServicePid()
    {
        var pid = ServiceSetup.QueryServiceProcessId();
        if (pid == 0)
        {
            return; // service not installed/running here — nothing to assert
        }
        Assert.True(PipeServerIdentity.IsServiceProcess(pid));
    }

    [Fact]
    public async Task IsServerTrusted_InProcessFakeServer_IsFalse()
    {
        using var server = new FakePipeServer();
        using var stream = new NamedPipeClientStream(
            ".", server.PipeName, PipeDirection.InOut, PipeOptions.Asynchronous);
        await stream.ConnectAsync(new CancellationTokenSource(5000).Token);

        // The server end is this test process, not the fmf-engine service.
        Assert.False(PipeServerIdentity.IsServerTrusted(stream.SafePipeHandle));
    }
}
