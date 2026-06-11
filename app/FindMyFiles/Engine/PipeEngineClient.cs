using System.Collections.Concurrent;
using System.Diagnostics;
using System.IO.Pipes;
using System.Text;
using System.Text.Json;
using FindMyFiles.Services;

namespace FindMyFiles.Engine;

/// <summary>
/// Engine client over the fmf-service named pipe (docs/ARCHITECTURE.md
/// 「Pipe プロトコル」). A resident supervisor task owns the connection:
/// connect → Hello (version check; a mismatch is fatal) → Subscribe →
/// IndexStatus (synthesized VolumeUpdated + IndexChanged) → Connected. On
/// disconnect every pending request fails fast with
/// <see cref="EngineUnavailableException"/>, live results turn stale via an
/// epoch bump, and reconnection retries forever with 250ms→5s backoff.
/// Events fire on the read-loop thread — consumers marshal, same contract as
/// the FFI client. No DispatcherQueue dependency.
/// </summary>
public sealed class PipeEngineClient : IEngineClient
{
    private static readonly TimeSpan InitialBackoff = TimeSpan.FromMilliseconds(250);
    private static readonly TimeSpan MaxBackoff = TimeSpan.FromSeconds(5);

    private static readonly JsonSerializerOptions JsonOpts = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.SnakeCaseLower,
    };

    private readonly string _pipeName;
    private readonly CancellationTokenSource _cts = new();
    private readonly SemaphoreSlim _writeLock = new(1, 1);
    private readonly ConcurrentDictionary<
        uint, TaskCompletionSource<(int Status, byte[] Payload)>> _pending = new();
    private readonly object _statsLock = new();

    private NamedPipeClientStream? _stream;
    private Task? _supervisor;
    private int _requestId;
    private int _epoch;
    private int _disposed;
    private EngineConnectionState _connection = EngineConnectionState.Connecting;
    private long _reconnects;
    private double _pageRttEwmaUs;
    private uint _serverPid;
    private uint _abiVersion;

    /// <summary>Per-request deadline; a breach means the transport is gone.</summary>
    internal TimeSpan RequestTimeout { get; set; } = TimeSpan.FromSeconds(10);

    public event Action<string>? IndexChanged;
    public event Action<VolumeStatus>? VolumeUpdated;
    public event Action<int>? EngineErrorOccurred;
    public event Action<EngineConnectionState>? ConnectionChanged;

    public EngineConnectionState Connection => _connection;

    public PipeEngineClient(string pipeName = PipeProtocol.DefaultPipeName)
        : this(pipeName, autoStart: true)
    {
    }

    /// <summary>Tests pass autoStart=false to attach event handlers before
    /// the supervisor races them to the first connection.</summary>
    internal PipeEngineClient(string pipeName, bool autoStart)
    {
        _pipeName = ToShortName(pipeName);
        if (autoStart)
        {
            Start();
        }
    }

    internal void Start() =>
        _supervisor ??= Task.Run(() => SuperviseAsync(_cts.Token), CancellationToken.None);

    /// <summary>Accepts both the full path (\\.\pipe\name) and the short name.</summary>
    private static string ToShortName(string pipeName)
    {
        const string prefix = @"\\.\pipe\";
        return pipeName.StartsWith(prefix, StringComparison.OrdinalIgnoreCase)
            ? pipeName[prefix.Length..]
            : pipeName;
    }

    /// <summary>Can a server be reached and Hello'd on this pipe within the
    /// timeout? Used by the factory's `auto` mode (250ms budget).</summary>
    public static bool Probe(string pipeName, TimeSpan timeout)
    {
        try
        {
            return ProbeAsync(pipeName, timeout).GetAwaiter().GetResult();
        }
        catch
        {
            return false;
        }
    }

    internal static async Task<bool> ProbeAsync(string pipeName, TimeSpan timeout)
    {
        using var cts = new CancellationTokenSource(timeout);
        try
        {
            using var stream = new NamedPipeClientStream(
                ".", ToShortName(pipeName), PipeDirection.InOut, PipeOptions.Asynchronous);
            await stream.ConnectAsync(cts.Token).ConfigureAwait(false);
            var frame = PipeProtocol.EncodeFrame(
                PipeProtocol.Op.Hello, 0, 1, 0,
                PipeProtocol.EncodeHelloReq(PipeProtocol.ProtocolVersion));
            await stream.WriteAsync(frame, cts.Token).ConfigureAwait(false);
            var header = new byte[PipeProtocol.HeaderLen];
            await stream.ReadExactlyAsync(header, cts.Token).ConfigureAwait(false);
            var h = PipeProtocol.ReadHeader(header);
            var payload = new byte[h.Len];
            if (h.Len > 0)
            {
                await stream.ReadExactlyAsync(payload, cts.Token).ConfigureAwait(false);
            }
            if (h.StatusCode != PipeProtocol.Status.Ok)
            {
                return false;
            }
            var (version, _, _) = PipeProtocol.DecodeHelloResp(payload);
            return version == PipeProtocol.ProtocolVersion;
        }
        catch
        {
            return false;
        }
    }

    // ── Connection supervisor ───────────────────────────────────────────

    private async Task SuperviseAsync(CancellationToken ct)
    {
        var backoff = InitialBackoff;
        var everConnected = false;
        while (!ct.IsCancellationRequested)
        {
            NamedPipeClientStream? stream = null;
            try
            {
                stream = new NamedPipeClientStream(
                    ".", _pipeName, PipeDirection.InOut, PipeOptions.Asynchronous);
                await stream.ConnectAsync(ct).ConfigureAwait(false);
                Volatile.Write(ref _stream, stream);
                var readLoop = Task.Run(() => ReadLoopAsync(stream, ct), CancellationToken.None);
                await HandshakeAsync(ct).ConfigureAwait(false);
                if (everConnected)
                {
                    Interlocked.Increment(ref _reconnects);
                }
                everConnected = true;
                backoff = InitialBackoff;
                SetConnection(EngineConnectionState.Connected);
                await readLoop.ConfigureAwait(false); // returns when the pipe dies
            }
            catch (OperationCanceledException) when (ct.IsCancellationRequested)
            {
                break;
            }
            catch (ProtocolMismatchException ex)
            {
                // A version skew never fixes itself by retrying — stay down
                // until someone updates one side (per the pipe spec).
                FileLog.Error("pipe", $"fatal protocol mismatch — not reconnecting: {ex.Message}");
                TearDownConnection(stream);
                return;
            }
            catch (Exception ex)
            {
                FileLog.Warn("pipe", $"connection attempt failed: {ex.Message}");
            }
            TearDownConnection(stream);
            SetConnection(everConnected
                ? EngineConnectionState.Reconnecting
                : EngineConnectionState.Connecting);
            try
            {
                await Task.Delay(backoff, ct).ConfigureAwait(false);
            }
            catch (OperationCanceledException)
            {
                break;
            }
            backoff = TimeSpan.FromTicks(Math.Min(backoff.Ticks * 2, MaxBackoff.Ticks));
        }
        TearDownConnection(null);
    }

    /// <summary>Fixed (re)connect sequence — the pipe spec is canonical:
    /// Hello → Subscribe → IndexStatus → synthesized events.</summary>
    private async Task HandshakeAsync(CancellationToken ct)
    {
        var (status, payload) = await RequestAsync(
            PipeProtocol.Op.Hello,
            PipeProtocol.EncodeHelloReq(PipeProtocol.ProtocolVersion), ct).ConfigureAwait(false);
        if (status == PipeProtocol.Status.InvalidArg)
        {
            throw new ProtocolMismatchException(
                $"server rejected protocol version {PipeProtocol.ProtocolVersion}: {Detail(payload)}");
        }
        if (status != PipeProtocol.Status.Ok)
        {
            throw new EngineUnavailableException($"Hello failed ({status}): {Detail(payload)}");
        }
        var (serverVersion, abiVersion, serverPid) = PipeProtocol.DecodeHelloResp(payload);
        if (serverVersion != PipeProtocol.ProtocolVersion)
        {
            throw new ProtocolMismatchException(
                $"server speaks protocol {serverVersion}, this client speaks {PipeProtocol.ProtocolVersion}");
        }
        lock (_statsLock)
        {
            _serverPid = serverPid;
            _abiVersion = abiVersion;
        }

        (status, payload) = await RequestAsync(PipeProtocol.Op.Subscribe, [], ct)
            .ConfigureAwait(false);
        if (status != PipeProtocol.Status.Ok)
        {
            throw new EngineUnavailableException($"Subscribe failed ({status}): {Detail(payload)}");
        }

        (status, payload) = await RequestAsync(PipeProtocol.Op.IndexStatus, [], ct)
            .ConfigureAwait(false);
        if (status != PipeProtocol.Status.Ok)
        {
            throw new EngineUnavailableException($"IndexStatus failed ({status}): {Detail(payload)}");
        }

        // Synthesized catch-up: VolumeUpdated per volume from the status
        // snapshot, then one local IndexChanged so a requery picks up
        // whatever happened while disconnected (the server sends neither).
        foreach (var s in PipeProtocol.DecodeVolumeStatuses(payload))
        {
            RaiseSafe(() => VolumeUpdated?.Invoke(s), "VolumeUpdated");
        }
        RaiseSafe(() => IndexChanged?.Invoke("*"), "IndexChanged");
    }

    private async Task ReadLoopAsync(NamedPipeClientStream stream, CancellationToken ct)
    {
        var header = new byte[PipeProtocol.HeaderLen];
        try
        {
            while (!ct.IsCancellationRequested)
            {
                await stream.ReadExactlyAsync(header, ct).ConfigureAwait(false);
                var h = PipeProtocol.ReadHeader(header); // oversize throws → drop the link
                var payload = new byte[h.Len];
                if (h.Len > 0)
                {
                    await stream.ReadExactlyAsync(payload, ct).ConfigureAwait(false);
                }
                if (h.IsEvent)
                {
                    DispatchEvent(payload);
                }
                else if (h.IsResponse)
                {
                    if (_pending.TryRemove(h.RequestId, out var tcs))
                    {
                        tcs.TrySetResult((h.StatusCode, payload));
                    }
                }
                else
                {
                    throw new InvalidDataException(
                        $"frame with neither response nor event flag (opcode {h.Opcode})");
                }
            }
        }
        catch (OperationCanceledException)
        {
        }
        catch (EndOfStreamException)
        {
            FileLog.Warn("pipe", "server closed the connection");
        }
        catch (Exception ex)
        {
            FileLog.Warn("pipe", $"read loop ended: {ex.Message}");
        }
    }

    /// <summary>Event pushes fire handlers on this (read-loop) thread; the
    /// same contract as FFI engine threads — consumers marshal.</summary>
    private void DispatchEvent(byte[] payload)
    {
        var (kind, entries, volume) = PipeProtocol.DecodeEvent(payload);
        switch (kind)
        {
            case 1:
                RaiseSafe(
                    () => VolumeUpdated?.Invoke(
                        new VolumeStatus(volume, VolumeState.Scanning, entries)),
                    "VolumeUpdated");
                break;
            case 2:
                RaiseSafe(
                    () => VolumeUpdated?.Invoke(new VolumeStatus(volume, VolumeState.Ready, entries)),
                    "VolumeUpdated");
                break;
            case 3:
                RaiseSafe(() => IndexChanged?.Invoke(volume), "IndexChanged");
                break;
            case 4:
                RaiseSafe(
                    () => VolumeUpdated?.Invoke(new VolumeStatus(volume, VolumeState.Rescanning, 0)),
                    "VolumeUpdated");
                break;
            case 5:
                RaiseSafe(
                    () => VolumeUpdated?.Invoke(new VolumeStatus(volume, VolumeState.Failed, 0)),
                    "VolumeUpdated");
                break;
            case 6:
                RaiseSafe(() => EngineErrorOccurred?.Invoke((int)entries), "EngineErrorOccurred");
                break;
            default:
                FileLog.Warn("pipe", $"unknown event kind {kind}");
                break;
        }
    }

    /// <summary>A faulting consumer must not kill the read loop (落ちない).</summary>
    private static void RaiseSafe(Action raise, string what)
    {
        try
        {
            raise();
        }
        catch (Exception ex)
        {
            FileLog.Error("pipe", $"{what} handler failed", ex);
        }
    }

    private void TearDownConnection(NamedPipeClientStream? stream)
    {
        var active = Interlocked.Exchange(ref _stream, null);
        if (active is not null)
        {
            // Results born on this connection can never be paged again.
            Interlocked.Increment(ref _epoch);
        }
        SafeDispose(active);
        if (!ReferenceEquals(active, stream))
        {
            SafeDispose(stream);
        }
        foreach (var id in _pending.Keys)
        {
            if (_pending.TryRemove(id, out var tcs))
            {
                tcs.TrySetException(
                    new EngineUnavailableException("engine service connection lost"));
            }
        }
    }

    private static void SafeDispose(IDisposable? d)
    {
        try
        {
            d?.Dispose();
        }
        catch
        {
            // Already broken — nothing to report.
        }
    }

    private void SetConnection(EngineConnectionState state)
    {
        if (_connection == state)
        {
            return;
        }
        _connection = state;
        RaiseSafe(() => ConnectionChanged?.Invoke(state), "ConnectionChanged");
    }

    // ── Request plumbing ────────────────────────────────────────────────

    private async Task<(int Status, byte[] Payload)> RequestAsync(
        ushort opcode, byte[] payload, CancellationToken ct = default)
    {
        var stream = Volatile.Read(ref _stream)
            ?? throw new EngineUnavailableException("engine service is not connected");
        var id = unchecked((uint)Interlocked.Increment(ref _requestId));
        var tcs = new TaskCompletionSource<(int Status, byte[] Payload)>(
            TaskCreationOptions.RunContinuationsAsynchronously);
        _pending[id] = tcs;
        try
        {
            var frame = PipeProtocol.EncodeFrame(opcode, 0, id, 0, payload);
            await _writeLock.WaitAsync(ct).ConfigureAwait(false);
            try
            {
                await stream.WriteAsync(frame, ct).ConfigureAwait(false);
            }
            finally
            {
                _writeLock.Release();
            }
            return await tcs.Task.WaitAsync(RequestTimeout, ct).ConfigureAwait(false);
        }
        catch (TimeoutException)
        {
            throw new EngineUnavailableException(
                $"request (opcode {opcode}) timed out after {RequestTimeout.TotalSeconds:F0}s");
        }
        catch (Exception ex) when (ex is IOException or ObjectDisposedException)
        {
            throw new EngineUnavailableException($"engine service connection lost: {ex.Message}");
        }
        finally
        {
            _pending.TryRemove(id, out _);
        }
    }

    /// <summary>Request + FFI-equivalent status mapping (error responses
    /// carry the detail text inline).</summary>
    private async Task<byte[]> RequestOkAsync(
        ushort opcode, byte[] payload, string operation, CancellationToken ct = default)
    {
        var (status, resp) = await RequestAsync(opcode, payload, ct).ConfigureAwait(false);
        if (status != PipeProtocol.Status.Ok)
        {
            throw status switch
            {
                PipeProtocol.Status.QuerySyntax => new QuerySyntaxException(Detail(resp)),
                PipeProtocol.Status.Stale => new StaleResultException(),
                _ => new EngineException($"{operation} failed ({status}): {Detail(resp)}", status),
            };
        }
        return resp;
    }

    private static string Detail(byte[] payload) => Encoding.UTF8.GetString(payload);

    // ── IEngineClient ───────────────────────────────────────────────────

    public async Task<IReadOnlyList<string>> ListVolumesAsync()
    {
        var payload = await RequestOkAsync(PipeProtocol.Op.ListVolumes, [], "ListVolumes")
            .ConfigureAwait(false);
        return [.. PipeProtocol.DecodeVolumeStatuses(payload).Select(s => s.Label)];
    }

    public async Task StartIndexingAsync(IReadOnlyList<string> volumes)
    {
        await RequestOkAsync(
            PipeProtocol.Op.IndexStart, PipeProtocol.EncodeIndexStartReq(volumes), "IndexStart")
            .ConfigureAwait(false);
    }

    public async Task<IReadOnlyList<VolumeStatus>> GetStatusAsync()
    {
        var payload = await RequestOkAsync(PipeProtocol.Op.IndexStatus, [], "IndexStatus")
            .ConfigureAwait(false);
        return PipeProtocol.DecodeVolumeStatuses(payload);
    }

    public async Task<SearchOutcome> SearchAsync(string query, SearchOptions options)
    {
        var resp = await RequestOkAsync(
            PipeProtocol.Op.Query, PipeProtocol.EncodeQueryReq(options, query), "Query")
            .ConfigureAwait(false);
        var (resultId, count, traceJson) = PipeProtocol.DecodeQueryResp(resp);
        QueryTraceData? trace = null;
        if (traceJson.Length > 0)
        {
            trace = JsonSerializer.Deserialize<QueryTraceData>(traceJson, JsonOpts);
        }
        return new SearchOutcome(
            new PipeSearchResult(this, resultId, (long)count, CurrentEpoch), trace);
    }

    public async Task<EngineStatsData?> GetStatsAsync()
    {
        byte[] payload;
        try
        {
            int status;
            (status, payload) = await RequestAsync(PipeProtocol.Op.Stats, []).ConfigureAwait(false);
            if (status != PipeProtocol.Status.Ok)
            {
                return null; // FFI parity: stats are best-effort
            }
        }
        catch (EngineUnavailableException ex)
        {
            FileLog.Warn("pipe", $"stats unavailable: {ex.Message}");
            return null;
        }
        var stats = JsonSerializer.Deserialize<EngineStatsData>(payload, JsonOpts);
        if (stats is not null)
        {
            lock (_statsLock)
            {
                stats.Transport = new TransportStatsData
                {
                    State = _connection.ToString(),
                    Reconnects = Interlocked.Read(ref _reconnects),
                    PageRttEwmaUs = _pageRttEwmaUs,
                    ServerPid = _serverPid,
                    AbiVersion = _abiVersion,
                };
            }
        }
        return stats;
    }

    // ── Result paging (used by PipeSearchResult) ────────────────────────

    internal int CurrentEpoch => Volatile.Read(ref _epoch);

    internal async Task<IReadOnlyList<RowData>> FetchPageAsync(
        ulong resultId, long offset, int count)
    {
        var start = Stopwatch.GetTimestamp();
        var payload = await RequestOkAsync(
            PipeProtocol.Op.ResultPage,
            PipeProtocol.EncodeResultPageReq(resultId, (ulong)offset, (uint)count),
            "ResultPage").ConfigureAwait(false);
        var rttUs = Stopwatch.GetElapsedTime(start).TotalMicroseconds;
        lock (_statsLock)
        {
            _pageRttEwmaUs = _pageRttEwmaUs == 0 ? rttUs : 0.8 * _pageRttEwmaUs + 0.2 * rttUs;
        }
        return PipeProtocol.DecodePageResp(payload);
    }

    internal void ReleaseResult(ulong resultId, int epoch)
    {
        if (epoch != CurrentEpoch || Volatile.Read(ref _stream) is null)
        {
            return; // the server freed it together with the dead connection
        }
        ReleaseResultAsync(resultId).Forget("pipe.release");
    }

    private async Task ReleaseResultAsync(ulong resultId)
    {
        try
        {
            await RequestOkAsync(
                PipeProtocol.Op.ResultFree, PipeProtocol.EncodeResultFreeReq(resultId), "ResultFree")
                .ConfigureAwait(false);
        }
        catch (EngineUnavailableException)
        {
            // Disconnected mid-release: the per-connection registry on the
            // server already freed it. Not an error worth surfacing.
        }
    }

    public void Dispose()
    {
        if (Interlocked.Exchange(ref _disposed, 1) != 0)
        {
            return;
        }
        // Stop the supervisor and break the stream; never block shutdown on
        // the background task. The CTS stays undisposed on purpose — the
        // supervisor may still observe the token after we return.
        _cts.Cancel();
        TearDownConnection(null);
    }

    private sealed class ProtocolMismatchException(string message) : Exception(message);
}

/// <summary>
/// Pipe-backed <see cref="ISearchResult"/>. Pages stale out when the
/// connection epoch moves (disconnects); Dispose defers the wire-level
/// ResultFree until every in-flight page fetch has drained.
/// </summary>
internal sealed class PipeSearchResult(
    PipeEngineClient client, ulong resultId, long count, int epoch) : ISearchResult
{
    private int _inFlight;
    private int _released;
    private volatile bool _disposed;

    public long Count { get; } = count;

    public async Task<IReadOnlyList<RowData>> GetRangeAsync(long offset, int count)
    {
        if (_disposed || epoch != client.CurrentEpoch)
        {
            throw new StaleResultException();
        }
        Interlocked.Increment(ref _inFlight);
        try
        {
            if (epoch != client.CurrentEpoch)
            {
                throw new StaleResultException(); // re-check inside the guard
            }
            return await client.FetchPageAsync(resultId, offset, count).ConfigureAwait(false);
        }
        finally
        {
            if (Interlocked.Decrement(ref _inFlight) == 0 && _disposed)
            {
                MaybeRelease();
            }
        }
    }

    public void Dispose()
    {
        _disposed = true;
        if (Volatile.Read(ref _inFlight) == 0)
        {
            MaybeRelease();
        }
    }

    private void MaybeRelease()
    {
        if (Interlocked.Exchange(ref _released, 1) == 0)
        {
            client.ReleaseResult(resultId, epoch);
        }
    }
}
