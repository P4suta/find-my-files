using System.IO.Pipes;
using FindMyFiles.Engine;
using Xunit;

namespace FindMyFiles.Tests;

/// <summary>
/// <see cref="PipeConnection"/> in isolation, over a raw in-test pipe pair:
/// the connection object owns its own life — a write racing teardown is
/// answered by the object (normalized transport exception), which is the
/// structural replacement for the old "volatile null-check, then write"
/// convention in PipeEngineClient.
/// </summary>
public sealed class PipeConnectionTests
{
    private static async Task<(NamedPipeServerStream Server, NamedPipeClientStream Client)>
        ConnectedPairAsync()
    {
        var name = "fmf-conn-test-" + Guid.NewGuid().ToString("N");
        var server = new NamedPipeServerStream(
            name, PipeDirection.InOut, 1, PipeTransmissionMode.Byte, PipeOptions.Asynchronous);
        var client = new NamedPipeClientStream(
            ".", name, PipeDirection.InOut, PipeOptions.Asynchronous);
        var accept = server.WaitForConnectionAsync();
        await client.ConnectAsync();
        await accept;
        return (server, client);
    }

    private static PipeConnection Wrap(NamedPipeClientStream client, int epoch = 1) =>
        new(client, epoch, _ => { }, (_, _, _) => { }, CancellationToken.None);

    [Fact]
    public async Task WriteAfterDispose_IsNormalizedToEngineUnavailable()
    {
        var (server, client) = await ConnectedPairAsync();
        using (server)
        {
            var conn = Wrap(client);
            conn.Dispose(); // the supervisor tore this connection down

            // The grabbed object answers its own liveness: the racing writer
            // gets the transport exception, never a raw
            // ObjectDisposedException leaking from the dead stream.
            var frame = PipeProtocol.EncodeFrame(PipeProtocol.Op.Stats, 0, 1, 0, []);
            await Assert.ThrowsAsync<EngineUnavailableException>(
                () => conn.WriteFrameAsync(frame, CancellationToken.None));
        }
    }

    [Fact]
    public async Task ServerVanishing_CompletesTheReadLoopQuietly_AndDisposeIsIdempotent()
    {
        var (server, client) = await ConnectedPairAsync();
        var conn = Wrap(client);

        server.Dispose(); // server side goes away under the connection
        // The supervisor awaits ReadLoop to know the connection died — it
        // must complete (never fault, never hang).
        await conn.ReadLoop.WaitAsync(TimeSpan.FromSeconds(5));

        conn.Dispose();
        conn.Dispose(); // double-dispose must be harmless
        await Assert.ThrowsAsync<EngineUnavailableException>(
            () => conn.WriteFrameAsync(
                PipeProtocol.EncodeFrame(PipeProtocol.Op.Stats, 0, 1, 0, []),
                CancellationToken.None));
    }

    [Fact]
    public async Task BrokenPipeDuringWrite_IsNormalizedToEngineUnavailable()
    {
        var (server, client) = await ConnectedPairAsync();
        var conn = Wrap(client);
        server.Dispose(); // not our Dispose: the far end died (IOException path)
        await conn.ReadLoop.WaitAsync(TimeSpan.FromSeconds(5));

        var frame = PipeProtocol.EncodeFrame(PipeProtocol.Op.Stats, 0, 1, 0, []);
        await Assert.ThrowsAsync<EngineUnavailableException>(
            () => conn.WriteFrameAsync(frame, CancellationToken.None));
        conn.Dispose();
    }
}
