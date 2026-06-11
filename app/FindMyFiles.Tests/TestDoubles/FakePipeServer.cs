using System.IO.Pipes;
using FindMyFiles.Engine;

namespace FindMyFiles.Tests.TestDoubles;

/// <summary>
/// In-test named pipe server speaking the fmf wire protocol on a unique pipe
/// name. Default handlers answer every opcode from scriptable data
/// (<see cref="Statuses"/>, <see cref="Rows"/>); <see cref="Handler"/>
/// overrides individual requests (return null to fall through), and
/// <see cref="ChunkedWrites"/> writes responses one byte at a time to prove
/// the client reassembles frames across arbitrary boundaries. Requests are
/// handled concurrently (out-of-order completion is wire-legal), so a held
/// response never blocks the next incoming frame.
///
/// Mimicry scope and guarantees: the wire is faithful — every frame this
/// server emits is well-formed (PipeProtocol codecs both ways), the
/// handshake sequence, response correlation and event push match
/// fmf-service. The semantics behind the wire are deliberately shallow:
/// Query ignores the query text and always answers Ok from
/// <see cref="Rows"/> (no syntax verdicts), results never go stale on their
/// own, nothing is indexed. Tests that need behavioral semantics (e.g. the
/// contract suite's QuerySyntax case) script them through
/// <see cref="Handler"/>; tests that need the real semantics use the real
/// service (FMF_PIPE_TESTS).
/// </summary>
public sealed class FakePipeServer : IDisposable
{
    private sealed class Conn
    {
        public required NamedPipeServerStream Stream { get; init; }
        public SemaphoreSlim WriteLock { get; } = new(1, 1);
    }

    private readonly CancellationTokenSource _cts = new();
    private readonly object _gate = new();
    private readonly List<Conn> _conns = [];
    private readonly List<(int Connection, ushort Opcode, byte[] Payload)> _received = [];
    private int _connections;

    public string PipeName { get; } = "fmf-test-" + Guid.NewGuid().ToString("N");

    public uint ProtocolVersion { get; set; } = PipeProtocol.ProtocolVersion;
    public uint AbiVersion { get; set; } = 1;
    public uint ServerPid { get; set; } = 4242;

    /// <summary>Write every response one byte at a time.</summary>
    public bool ChunkedWrites { get; set; }

    /// <summary>Answer for ListVolumes and IndexStatus.</summary>
    public List<VolumeStatus> Statuses { get; set; } = [new("C:", VolumeState.Ready, 42)];

    /// <summary>Result set every Query answers with; ResultPage slices it.</summary>
    public List<RowData> Rows { get; set; } = [];

    /// <summary>Per-request override: a non-null task short-circuits the
    /// default handler (and may be held open by the test).</summary>
    public Func<ushort, byte[], Task<(int Status, byte[] Payload)>?>? Handler { get; set; }

    public int ConnectionCount => Volatile.Read(ref _connections);

    public FakePipeServer()
    {
        Task.Run(AcceptLoopAsync);
    }

    public IReadOnlyList<(int Connection, ushort Opcode, byte[] Payload)> Received
    {
        get
        {
            lock (_gate)
            {
                return [.. _received];
            }
        }
    }

    /// <summary>Request opcodes seen on one connection, in arrival order.</summary>
    public List<ushort> OpcodesOf(int connection)
    {
        lock (_gate)
        {
            return [.. _received.Where(r => r.Connection == connection).Select(r => r.Opcode)];
        }
    }

    /// <summary>Polls until `opcode` has arrived `minCount` times.</summary>
    public async Task WaitForAsync(ushort opcode, int minCount = 1, int timeoutMs = 5000)
    {
        var deadline = Environment.TickCount64 + timeoutMs;
        while (true)
        {
            int n;
            lock (_gate)
            {
                n = _received.Count(r => r.Opcode == opcode);
            }
            if (n >= minCount)
            {
                return;
            }
            if (Environment.TickCount64 > deadline)
            {
                throw new TimeoutException($"opcode {opcode} arrived {n} < {minCount} times");
            }
            await Task.Delay(10);
        }
    }

    /// <summary>Pushes an event frame to the most recent live connection.</summary>
    public async Task SendEventAsync(uint kind, ulong entries, string volume)
    {
        Conn? conn;
        lock (_gate)
        {
            conn = _conns.LastOrDefault(c => c.Stream.IsConnected);
        }
        if (conn is null)
        {
            throw new InvalidOperationException("no connected client to push to");
        }
        await SendAsync(
            conn, (ushort)kind, PipeProtocol.FlagEvent, 0, 0,
            PipeProtocol.EncodeEvent(kind, entries, volume));
    }

    /// <summary>Hard-drops every live connection (the accept loop keeps
    /// running, so clients can reconnect).</summary>
    public void DisconnectAll()
    {
        lock (_gate)
        {
            foreach (var c in _conns)
            {
                try
                {
                    c.Stream.Dispose();
                }
                catch
                {
                }
            }
            _conns.Clear();
        }
    }

    public void Dispose()
    {
        _cts.Cancel();
        DisconnectAll();
    }

    private async Task AcceptLoopAsync()
    {
        while (!_cts.IsCancellationRequested)
        {
            NamedPipeServerStream? stream = null;
            try
            {
                stream = new NamedPipeServerStream(
                    PipeName, PipeDirection.InOut,
                    NamedPipeServerStream.MaxAllowedServerInstances,
                    PipeTransmissionMode.Byte, PipeOptions.Asynchronous);
                await stream.WaitForConnectionAsync(_cts.Token);
            }
            catch
            {
                stream?.Dispose();
                return;
            }
            var conn = new Conn { Stream = stream };
            lock (_gate)
            {
                _conns.Add(conn);
            }
            var index = Interlocked.Increment(ref _connections) - 1;
            _ = Task.Run(() => ServeAsync(conn, index));
        }
    }

    private async Task ServeAsync(Conn conn, int index)
    {
        var header = new byte[PipeProtocol.HeaderLen];
        try
        {
            while (!_cts.IsCancellationRequested)
            {
                await conn.Stream.ReadExactlyAsync(header, _cts.Token);
                var h = PipeProtocol.ReadHeader(header);
                var payload = new byte[h.Len];
                if (h.Len > 0)
                {
                    await conn.Stream.ReadExactlyAsync(payload, _cts.Token);
                }
                lock (_gate)
                {
                    _received.Add((index, h.Opcode, payload));
                }
                // Concurrent dispatch: a held response (Handler gate) must
                // not stop the server from reading the next request.
                _ = Task.Run(async () =>
                {
                    try
                    {
                        var (status, resp) = await HandleAsync(h.Opcode, payload);
                        await SendAsync(
                            conn, h.Opcode, PipeProtocol.FlagResponse, h.RequestId, status, resp);
                    }
                    catch
                    {
                        // Connection torn down mid-response — fine in tests.
                    }
                });
            }
        }
        catch
        {
            // Client went away or the test dropped the connection.
        }
    }

    private async Task<(int Status, byte[] Payload)> HandleAsync(ushort opcode, byte[] payload)
    {
        if (Handler?.Invoke(opcode, payload) is { } overridden)
        {
            return await overridden;
        }
        switch (opcode)
        {
            case PipeProtocol.Op.Hello:
                return (PipeProtocol.Status.Ok,
                    PipeProtocol.EncodeHelloResp(ProtocolVersion, AbiVersion, ServerPid));
            case PipeProtocol.Op.Subscribe:
            case PipeProtocol.Op.Unsubscribe:
            case PipeProtocol.Op.IndexStart:
            case PipeProtocol.Op.ResultFree:
                return (PipeProtocol.Status.Ok, []);
            case PipeProtocol.Op.ListVolumes:
            case PipeProtocol.Op.IndexStatus:
                return (PipeProtocol.Status.Ok, PipeProtocol.EncodeVolumeStatuses(Statuses));
            case PipeProtocol.Op.Query:
                return (PipeProtocol.Status.Ok,
                    PipeProtocol.EncodeQueryResp(1, (ulong)Rows.Count, "{}"));
            case PipeProtocol.Op.ResultPage:
            {
                var (_, offset, count) = PipeProtocol.DecodeResultPageReq(payload);
                var start = (int)Math.Min(offset, (ulong)Rows.Count);
                var n = Math.Min((int)count, Rows.Count - start);
                return (PipeProtocol.Status.Ok,
                    PipeProtocol.EncodePageResp(Rows.GetRange(start, n)));
            }
            case PipeProtocol.Op.Stats:
                return (PipeProtocol.Status.Ok, "{}"u8.ToArray());
            default:
                return (PipeProtocol.Status.InvalidArg, "unknown opcode"u8.ToArray());
        }
    }

    private async Task SendAsync(
        Conn conn, ushort opcode, ushort flags, uint requestId, int status, byte[] payload)
    {
        var frame = PipeProtocol.EncodeFrame(opcode, flags, requestId, status, payload);
        await conn.WriteLock.WaitAsync();
        try
        {
            if (ChunkedWrites)
            {
                for (var i = 0; i < frame.Length; i++)
                {
                    await conn.Stream.WriteAsync(frame.AsMemory(i, 1));
                    await conn.Stream.FlushAsync();
                }
            }
            else
            {
                await conn.Stream.WriteAsync(frame);
            }
        }
        finally
        {
            conn.WriteLock.Release();
        }
    }
}
