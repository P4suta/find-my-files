using System.Text;
using FindMyFiles.Engine;
using FindMyFiles.Tests.TestDoubles;
using Xunit.Abstractions;
using static FindMyFiles.Tests.TestDoubles.Polling;

namespace FindMyFiles.Tests.Contract;

/// <summary>
/// Contract run #2: <see cref="PipeEngineClient"/> over an in-test
/// <see cref="FakePipeServer"/>, always on. This exercises the full client
/// wire path (frames, correlation, epochs) hermetically; the QuerySyntax
/// verdicts are scripted onto the server from the same shared golden fixture
/// (FakePipeServer itself deliberately has no query semantics — see its
/// docstring). Staleness is forced the way production produces it: a dropped
/// connection turning the client epoch.
/// </summary>
public sealed class PipeOverFakeServerContractTests(ITestOutputHelper output)
    : EngineClientContractTests(output)
{
    private FakePipeServer? _server;

    protected override async Task<IEngineClient?> AcquireClientOrSkipAsync()
    {
        var invalid = GoldenInvalidQueries().ToHashSet(StringComparer.Ordinal);
        _server = new FakePipeServer
        {
            Rows = Rows.Many(8, "pipe"),
            Handler = (opcode, payload) =>
            {
                if (opcode == PipeProtocol.Op.Query)
                {
                    var (_, text) = PipeProtocol.DecodeQueryReq(payload);
                    if (invalid.Contains(text))
                    {
                        return Task.FromResult((
                            PipeProtocol.Status.QuerySyntax,
                            Encoding.UTF8.GetBytes($"syntax error (scripted fixture): {text}")));
                    }
                }
                return null; // default handlers
            },
        };
        var client = new PipeEngineClient(_server.PipeName);
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected,
            "connect to FakePipeServer");
        return client;
    }

    protected override Task TeardownAsync()
    {
        _server?.Dispose();
        return Task.CompletedTask;
    }

    protected override string ValidQuery => "pipe";

    protected override bool IsInProcTransport => false;

    protected override async Task<bool> TryForceStaleAsync(IEngineClient client)
    {
        var pipe = (PipeEngineClient)client;
        var epochBefore = pipe.CurrentEpoch;
        _server!.DisconnectAll();
        await WaitUntilAsync(
            () => pipe.CurrentEpoch != epochBefore, "connection epoch turn");
        await WaitUntilAsync(
            () => pipe.Connection == EngineConnectionState.Connected,
            "reconnect to FakePipeServer");
        return true;
    }
}
