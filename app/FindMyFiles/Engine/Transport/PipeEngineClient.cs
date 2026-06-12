using System.Collections.Concurrent;
using System.Diagnostics;
using System.IO.Pipes;
using System.Text;
using System.Text.Json;
using FindMyFiles.Services;

namespace FindMyFiles.Engine;

/// <summary>
/// Engine client over the fmf-service named pipe (docs/ARCHITECTURE.md
/// 「Pipe プロトコル」). This class is the connection *supervisor* plus the
/// request multiplexing table; the established connection itself (stream,
/// read loop, serialized writer, epoch) is one <see cref="PipeConnection"/>
/// object, replaced wholesale on every (re)connect. The supervisor loop:
/// connect → server-is-SYSTEM check (default pipe name only; SECURITY.md
/// 脅威4) → Hello (version check; a mismatch is fatal) → Subscribe →
/// IndexStatus (synthesized VolumeUpdated + IndexChanged) → Connected. On
/// disconnect every pending request fails fast with
/// <see cref="EngineUnavailableException"/>, live results turn stale because
/// their connection's epoch can never be current again, and reconnection
/// retries forever with 250ms→5s backoff. Events fire on the read-loop
/// thread — consumers marshal (see <see cref="EngineEventMarshaler"/>), same
/// contract as the FFI client. No DispatcherQueue dependency.
/// </summary>
public sealed class PipeEngineClient : IEngineClient
{
    private static readonly TimeSpan InitialBackoff = TimeSpan.FromMilliseconds(250);
    private static readonly TimeSpan MaxBackoff = TimeSpan.FromSeconds(5);

    private readonly string _pipeName;
    private readonly CancellationTokenSource _cts = new();
    private readonly ConcurrentDictionary<
        uint, TaskCompletionSource<(int Status, byte[] Payload)>> _pending = new();
    private readonly object _statsLock = new();

    private PipeConnection? _connection;
    private Task? _supervisor;
    private int _requestId;
    private int _epochSeq;
    private int _disposed;
    private EngineConnectionState _connectionState = EngineConnectionState.Connecting;
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

    public EngineConnectionState Connection => _connectionState;

    public PipeEngineClient(string pipeName = PipeProtocol.DefaultPipeName)
        : this(pipeName, autoStart: true)
    {
    }

    /// <summary>Server identity is verified on the default pipe name only;
    /// a custom --pipe-name (tests) skips the check (SECURITY.md 脅威4).</summary>
    private readonly bool _verifyServerIdentity;

    /// <summary>Tests pass autoStart=false to attach event handlers before
    /// the supervisor races them to the first connection.</summary>
    internal PipeEngineClient(string pipeName, bool autoStart)
    {
        _pipeName = ToShortName(pipeName);
        _verifyServerIdentity = _pipeName == PipeProtocol.DefaultPipeName;
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
            // Identification level is mandatory: the installed service's
            // verify_client ImpersonateNamedPipeClient's us to read our SID
            // against authorized_sids. The .NET default (None) yields an
            // anonymous token server-side → every connection is rejected
            // ("pipe client token rejected") — invisible to console-mode
            // tests where authorized_sids is empty and the check is skipped.
            using var stream = new NamedPipeClientStream(
                ".", ToShortName(pipeName), PipeDirection.InOut, PipeOptions.Asynchronous,
                System.Security.Principal.TokenImpersonationLevel.Identification);
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
                // Identification level: the service impersonates us to read
                // our SID for the authorized_sids check (see Probe). Without
                // it the server gets an anonymous token and rejects us.
                stream = new NamedPipeClientStream(
                    ".", _pipeName, PipeDirection.InOut, PipeOptions.Asynchronous,
                    System.Security.Principal.TokenImpersonationLevel.Identification);
                await stream.ConnectAsync(ct).ConfigureAwait(false);
                if (_verifyServerIdentity && !PipeServerIdentity.IsServerSystem(stream.SafePipeHandle))
                {
                    throw new ServerIdentityException(
                        $@"server on \\.\pipe\{_pipeName} is not running as SYSTEM "
                        + "— refusing to connect (possible pipe squatting; SECURITY.md 脅威4)");
                }
                var conn = new PipeConnection(
                    stream, Interlocked.Increment(ref _epochSeq), DispatchEvent, OnResponse, ct);
                stream = null; // owned by conn from here on
                Volatile.Write(ref _connection, conn);
                await HandshakeAsync(ct).ConfigureAwait(false);
                if (everConnected)
                {
                    Interlocked.Increment(ref _reconnects);
                }
                everConnected = true;
                backoff = InitialBackoff;
                SetConnection(EngineConnectionState.Connected);
                await conn.ReadLoop.ConfigureAwait(false); // returns when the pipe dies
            }
            catch (OperationCanceledException) when (ct.IsCancellationRequested)
            {
                break;
            }
            catch (FatalPipeException ex)
            {
                // A version skew or a non-SYSTEM impostor server never fixes
                // itself by retrying — stay down until a human fixes one side
                // (pipe spec / SECURITY.md 脅威4). Requests keep failing with
                // EngineUnavailableException.
                FileLog.Error("pipe", $"fatal pipe failure — not reconnecting: {ex.Message}");
                SafeDispose(stream);
                TearDownConnection();
                return;
            }
            catch (Exception ex)
            {
                FileLog.Warn("pipe", $"connection attempt failed: {ex.Message}");
            }
            SafeDispose(stream);
            TearDownConnection();
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
        TearDownConnection();
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

    /// <summary>Response frames from the connection's read loop land in the
    /// multiplexing table (out-of-order completion is wire-legal).</summary>
    private void OnResponse(uint requestId, int status, byte[] payload)
    {
        if (_pending.TryRemove(requestId, out var tcs))
        {
            tcs.TrySetResult((status, payload));
        }
    }

    /// <summary>Event pushes fire handlers on the read-loop thread; the
    /// same contract as FFI engine threads — consumers marshal.</summary>
    private void DispatchEvent(byte[] payload)
    {
        var (kind, entries, volume) = PipeProtocol.DecodeEvent(payload);
        switch ((EventKind)kind)
        {
            case EventKind.Progress:
                RaiseSafe(
                    () => VolumeUpdated?.Invoke(
                        new VolumeStatus(volume, VolumeState.Scanning, entries)),
                    "VolumeUpdated");
                break;
            case EventKind.VolumeReady:
                RaiseSafe(
                    () => VolumeUpdated?.Invoke(new VolumeStatus(volume, VolumeState.Ready, entries)),
                    "VolumeUpdated");
                break;
            case EventKind.IndexChanged:
                RaiseSafe(() => IndexChanged?.Invoke(volume), "IndexChanged");
                break;
            case EventKind.RescanStarted:
                RaiseSafe(
                    () => VolumeUpdated?.Invoke(new VolumeStatus(volume, VolumeState.Rescanning, 0)),
                    "VolumeUpdated");
                break;
            case EventKind.VolumeFailed:
                RaiseSafe(
                    () => VolumeUpdated?.Invoke(new VolumeStatus(volume, VolumeState.Failed, 0)),
                    "VolumeUpdated");
                break;
            case EventKind.EngineError: // entries = severity 1..3
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

    /// <summary>Retires the current connection object (its epoch can never
    /// be current again — results born on it are stale by construction) and
    /// fails every pending request fast.</summary>
    private void TearDownConnection()
    {
        Interlocked.Exchange(ref _connection, null)?.Dispose();
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
        if (_connectionState == state)
        {
            return;
        }
        _connectionState = state;
        RaiseSafe(() => ConnectionChanged?.Invoke(state), "ConnectionChanged");
    }

    // ── Request plumbing ────────────────────────────────────────────────

    private async Task<(int Status, byte[] Payload)> RequestAsync(
        ushort opcode, byte[] payload, CancellationToken ct = default)
    {
        // Grab the connection object once; from here on it answers its own
        // liveness (a write racing teardown surfaces as
        // EngineUnavailableException inside PipeConnection) — there is no
        // null-check-then-write window against a mutable stream field.
        var conn = Volatile.Read(ref _connection)
            ?? throw new EngineUnavailableException("engine service is not connected");
        var id = unchecked((uint)Interlocked.Increment(ref _requestId));
        var tcs = new TaskCompletionSource<(int Status, byte[] Payload)>(
            TaskCreationOptions.RunContinuationsAsynchronously);
        _pending[id] = tcs;
        // The caller's ct joins the client-lifetime token: either one aborts
        // the wait. Caller cancellation surfaces as OperationCanceledException;
        // a client-lifetime cancellation (Dispose) keeps reading as
        // EngineUnavailableException, same as before ct plumbing existed.
        using var linked = CancellationTokenSource.CreateLinkedTokenSource(_cts.Token, ct);
        try
        {
            var frame = PipeProtocol.EncodeFrame(opcode, 0, id, 0, payload);
            await conn.WriteFrameAsync(frame, linked.Token).ConfigureAwait(false);
            return await tcs.Task.WaitAsync(RequestTimeout, linked.Token).ConfigureAwait(false);
        }
        catch (TimeoutException)
        {
            throw new EngineUnavailableException(
                $"request (opcode {opcode}) timed out after {RequestTimeout.TotalSeconds:F0}s");
        }
        catch (OperationCanceledException) when (!ct.IsCancellationRequested)
        {
            throw new EngineUnavailableException("engine client disposed");
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

    public async Task<IReadOnlyList<string>> ListVolumesAsync(CancellationToken ct = default)
    {
        var payload = await RequestOkAsync(PipeProtocol.Op.ListVolumes, [], "ListVolumes", ct)
            .ConfigureAwait(false);
        return [.. PipeProtocol.DecodeVolumeStatuses(payload).Select(s => s.Label)];
    }

    public async Task StartIndexingAsync(
        IReadOnlyList<string> volumes, CancellationToken ct = default)
    {
        await RequestOkAsync(
            PipeProtocol.Op.IndexStart, PipeProtocol.EncodeIndexStartReq(volumes), "IndexStart", ct)
            .ConfigureAwait(false);
    }

    public async Task<IReadOnlyList<VolumeStatus>> GetStatusAsync(CancellationToken ct = default)
    {
        var payload = await RequestOkAsync(PipeProtocol.Op.IndexStatus, [], "IndexStatus", ct)
            .ConfigureAwait(false);
        return PipeProtocol.DecodeVolumeStatuses(payload);
    }

    public async Task<SearchOutcome> SearchAsync(
        string query, SearchOptions options, CancellationToken ct = default)
    {
        var resp = await RequestOkAsync(
            PipeProtocol.Op.Query, PipeProtocol.EncodeQueryReq(options, query), "Query", ct)
            .ConfigureAwait(false);
        var (resultId, count, traceJson) = PipeProtocol.DecodeQueryResp(resp);
        QueryTraceData? trace = null;
        if (traceJson.Length > 0)
        {
            trace = JsonSerializer.Deserialize<QueryTraceData>(traceJson, EngineJson.SnakeCase);
        }
        return new SearchOutcome(
            new PipeSearchResult(this, resultId, (long)count, CurrentEpoch), trace);
    }

    public async Task<EngineStatsData?> GetStatsAsync(CancellationToken ct = default)
    {
        byte[] payload;
        try
        {
            int status;
            (status, payload) = await RequestAsync(PipeProtocol.Op.Stats, [], ct)
                .ConfigureAwait(false);
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
        var stats = JsonSerializer.Deserialize<EngineStatsData>(payload, EngineJson.SnakeCase);
        if (stats is not null)
        {
            lock (_statsLock)
            {
                stats.Transport = new TransportStatsData
                {
                    State = _connectionState.ToString(),
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

    /// <summary>Epoch of the live connection; 0 (never a connection's value)
    /// while disconnected. A result is current iff its birth epoch equals
    /// this — connection generations are never reused, so a result born on a
    /// dead connection can never read as current again.</summary>
    internal int CurrentEpoch => Volatile.Read(ref _connection)?.Epoch ?? 0;

    internal async Task<IReadOnlyList<RowData>> FetchPageAsync(
        ulong resultId, long offset, int count, CancellationToken ct)
    {
        var start = Stopwatch.GetTimestamp();
        var payload = await RequestOkAsync(
            PipeProtocol.Op.ResultPage,
            PipeProtocol.EncodeResultPageReq(resultId, (ulong)offset, (uint)count),
            "ResultPage", ct).ConfigureAwait(false);
        var rttUs = Stopwatch.GetElapsedTime(start).TotalMicroseconds;
        lock (_statsLock)
        {
            _pageRttEwmaUs = _pageRttEwmaUs == 0 ? rttUs : 0.8 * _pageRttEwmaUs + 0.2 * rttUs;
        }
        return PipeProtocol.DecodePageResp(payload);
    }

    internal void ReleaseResult(ulong resultId, int epoch)
    {
        if (Volatile.Read(ref _connection) is not { } conn || conn.Epoch != epoch)
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
        // Stop the supervisor and break the connection; never block shutdown
        // on the background task. The CTS stays undisposed on purpose — the
        // supervisor may still observe the token after we return.
        _cts.Cancel();
        TearDownConnection();
    }

    /// <summary>Conditions a reconnect can never cure — the supervisor stops
    /// for good and every request fails with EngineUnavailableException.</summary>
    private class FatalPipeException(string message) : Exception(message);

    private sealed class ProtocolMismatchException(string message) : FatalPipeException(message);

    private sealed class ServerIdentityException(string message) : FatalPipeException(message);
}
