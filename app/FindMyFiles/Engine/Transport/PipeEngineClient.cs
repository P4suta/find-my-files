using System.Collections.Concurrent;
using System.Diagnostics;
using System.IO.Pipes;
using System.Text;
using System.Text.Json;
using FindMyFiles.Services;

namespace FindMyFiles.Engine;

/// <summary>
/// Engine client over the fmf-service named pipe (docs/ARCHITECTURE.md
/// "Pipe protocol"). This class is the connection *supervisor* plus the
/// request multiplexing table; the established connection itself (stream,
/// read loop, serialized writer, epoch) is one <see cref="PipeConnection"/>
/// object, replaced wholesale on every (re)connect. The supervisor loop:
/// connect → server-is-SYSTEM check (default pipe name only; SECURITY.md
/// Threat 4) → Hello (version check; a mismatch is fatal) → Subscribe →
/// IndexStatus (synthesized VolumeUpdated + IndexChanged) → Connected. On
/// disconnect every pending request fails fast with
/// <see cref="EngineUnavailableException"/>, live results turn stale because
/// their connection's epoch can never be current again, and reconnection
/// retries forever with 250ms→5s backoff. Events fire on the read-loop
/// thread — consumers marshal (see <see cref="EngineEventMarshaler"/>), same
/// contract as the FFI client. No DispatcherQueue dependency.
/// </summary>
internal sealed class PipeEngineClient : IEngineClient
{
    private static readonly TimeSpan InitialBackoff = TimeSpan.FromMilliseconds(250);
    private static readonly TimeSpan MaxBackoff = TimeSpan.FromSeconds(5);

    private readonly string _pipeName;
    private readonly CancellationTokenSource _cts = new();
    private readonly ConcurrentDictionary<uint, PendingRequest> _pending = new();

    private readonly System.Threading.Lock _statsLock = new();

    private PipeConnection? _connection;
    private Task? _supervisor;
    private int _requestId;
    private int _epochSeq;
    private int _disposed;
    private volatile EngineConnectionState _connectionState = EngineConnectionState.Connecting;
    private string? _terminalFailure;
    private long _reconnects;
    private double _pageRttEwmaUs;
    private uint _serverPid;
    private uint _abiVersion;

    private sealed record PendingRequest(
        ushort Opcode,
        TaskCompletionSource<(int Status, byte[] Payload)> Completion);

    /// <inheritdoc/>
    public EngineClientKind Kind => EngineClientKind.Service;

    /// <summary>Per-request deadline; a breach means the transport is gone.</summary>
    internal TimeSpan RequestTimeout { get; set; } = TimeSpan.FromSeconds(10);

    /// <inheritdoc/>
    public event Action<string>? IndexChanged;

    /// <inheritdoc/>
    public event Action<VolumeStatus>? VolumeUpdated;

    /// <inheritdoc/>
    public event Action<int>? EngineErrorOccurred;

    /// <inheritdoc/>
    public event Action<EngineConnectionState>? ConnectionChanged;

    /// <inheritdoc/>
    public EngineConnectionState Connection => _connectionState;

    /// <summary>Connects to the fmf-service named pipe and starts the
    /// supervisor loop immediately. <paramref name="pipeName"/> defaults to
    /// <see cref="PipeProtocol.DefaultPipeName"/>; only that default name has
    /// its server identity verified (SECURITY.md Threat 4) — a custom name
    /// (tests) skips the SYSTEM check.</summary>
    /// <param name="pipeName">Pipe to connect to, as either the short name or
    /// the full <c>\\.\pipe\…</c> path.</param>
    public PipeEngineClient(string pipeName = PipeProtocol.DefaultPipeName)
        : this(pipeName, autoStart: true)
    {
    }

    /// <summary>Server identity is verified on the default pipe name only;
    /// a custom --pipe-name (tests) skips the check (SECURITY.md Threat 4).</summary>
    private readonly bool _verifyServerIdentity;

    /// <summary>Tests pass autoStart=false to attach event handlers before
    /// the supervisor races them to the first connection.</summary>
    /// <param name="pipeName">Pipe to connect to, short name or full path.</param>
    /// <param name="autoStart">Whether to start the supervisor loop immediately.</param>
    internal PipeEngineClient(string pipeName, bool autoStart)
    {
        _pipeName = ToShortName(pipeName);
        _verifyServerIdentity = ShouldVerifyServerIdentity(_pipeName);
        if (autoStart)
        {
            Start();
        }
    }

    internal void Start() =>
        _supervisor ??= Task.Run(() => SuperviseAsync(_cts.Token), CancellationToken.None);

    /// <summary>Accepts both the full path (\\.\pipe\name) and the short name.</summary>
    /// <param name="pipeName">Pipe name as either the full path or short name.</param>
    private static string ToShortName(string pipeName)
    {
        const string prefix = @"\\.\pipe\";
        return pipeName.StartsWith(prefix, StringComparison.OrdinalIgnoreCase)
            ? pipeName[prefix.Length..]
            : pipeName;
    }

    /// <summary>Can a trusted server be reached and Hello'd on this pipe within
    /// the timeout? The default production pipe must belong to the
    /// SCM-registered service; custom test/development pipes skip that check.</summary>
    /// <param name="pipeName">Pipe to probe, short name or full path.</param>
    /// <param name="timeout">Budget for connect plus the Hello round-trip.</param>
    /// <returns>True if a protocol-compatible server answered within the timeout.</returns>
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
                ".",
                ToShortName(pipeName),
                PipeDirection.InOut,
                PipeOptions.Asynchronous,
                System.Security.Principal.TokenImpersonationLevel.Identification);
            await stream.ConnectAsync(cts.Token).ConfigureAwait(false);
            if (ShouldVerifyServerIdentity(pipeName)
                && !PipeServerIdentity.IsServerTrusted(stream.SafePipeHandle))
            {
                return false;
            }

            var frame = PipeProtocol.EncodeFrame(
                PipeProtocol.Op.Hello,
                0,
                1,
                0,
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

            if (h.Flags != PipeProtocol.FlagResponse
                || h.RequestId != 1
                || h.Opcode != PipeProtocol.Op.Hello
                || h.StatusCode != PipeProtocol.Status.Ok)
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

    /// <summary>Only the fixed production pipe has an SCM identity to verify.
    /// Kept separate so the full-path spelling and custom-pipe exception are
    /// pinned without requiring a live service.</summary>
    /// <param name="pipeName">Short or full pipe name.</param>
    /// <returns>True when the production service identity check is required.</returns>
    internal static bool ShouldVerifyServerIdentity(string pipeName) =>
        string.Equals(
            ToShortName(pipeName),
            PipeProtocol.DefaultPipeName,
            StringComparison.Ordinal);

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
                    ".",
                    _pipeName,
                    PipeDirection.InOut,
                    PipeOptions.Asynchronous,
                    System.Security.Principal.TokenImpersonationLevel.Identification);
                await stream.ConnectAsync(ct).ConfigureAwait(false);
                if (_verifyServerIdentity && !PipeServerIdentity.IsServerTrusted(stream.SafePipeHandle))
                {
                    throw new ServerIdentityException(
                        $@"server on \\.\pipe\{_pipeName} is not the registered fmf-engine service "
                        + "— refusing to connect (possible pipe squatting; SECURITY.md 脅威4)");
                }
#pragma warning disable CA2000 // owned by the client: stored and disposed on teardown/reconnect
                var conn = new PipeConnection(
                    stream, Interlocked.Increment(ref _epochSeq), DispatchEvent, OnResponse, ct);
#pragma warning restore CA2000
                stream = null; // owned by conn from here on
                Volatile.Write(ref _connection, conn);
                await HandshakeAsync(ct).ConfigureAwait(false);
                if (everConnected)
                {
                    // A successful *re*connect after a drop must leave a trace
                    // ("don't stay silent"): the first connect is announced by
                    // EngineClientFactory, so only reconnections are logged here
                    // — e.g. the service was killed and SCM restarted it. Failed
                    // attempts already log WARN; success used to log nothing.
                    var reconnects = Interlocked.Increment(ref _reconnects);
                    FileLog.Event("pipe", "reconnected to engine service", ("reconnects", reconnects));
                }

                everConnected = true;
                backoff = InitialBackoff;
                Volatile.Write(ref _terminalFailure, null);
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
                // (pipe spec / SECURITY.md Threat 4). Requests keep failing with
                // EngineUnavailableException.
                FileLog.Error("pipe", "fatal pipe failure — not reconnecting", ex);
                SafeDispose(stream);
                Volatile.Write(ref _terminalFailure, ex.Message);
                TearDownConnection();
                SetConnection(EngineConnectionState.Faulted);
                return;
            }
            catch (Exception ex)
            {
                FileLog.Warn("pipe", "connection attempt failed", ex);
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
    /// <param name="ct">Cancels the handshake on teardown or shutdown.</param>
    private async Task HandshakeAsync(CancellationToken ct)
    {
        var (status, payload, _) = await RequestAsync(
            PipeProtocol.Op.Hello,
            PipeProtocol.EncodeHelloReq(PipeProtocol.ProtocolVersion),
            ct).ConfigureAwait(false);
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

        (status, payload, _) = await RequestAsync(PipeProtocol.Op.Subscribe, [], ct)
            .ConfigureAwait(false);
        if (status != PipeProtocol.Status.Ok)
        {
            throw new EngineUnavailableException($"Subscribe failed ({status}): {Detail(payload)}");
        }

        (status, payload, _) = await RequestAsync(PipeProtocol.Op.IndexStatus, [], ct)
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
    /// <param name="requestId">Id of the pending request this frame answers.</param>
    /// <param name="opcode">Operation code echoed by the response.</param>
    /// <param name="status">Wire status code of the response.</param>
    /// <param name="payload">Response body bytes.</param>
    private void OnResponse(uint requestId, ushort opcode, int status, byte[] payload)
    {
        if (_pending.TryRemove(requestId, out var pending))
        {
            if (pending.Opcode != opcode)
            {
                pending.Completion.TrySetException(
                    new EngineUnavailableException(
                        $"response opcode mismatch for request {requestId}: "
                        + $"expected {pending.Opcode}, received {opcode}"));
                throw new InvalidDataException(
                    $"response opcode mismatch for request {requestId}");
            }

            pending.Completion.TrySetResult((status, payload));
            return;
        }

        // Caller cancellation/time-out can retire the pending wait after a
        // Query frame was already sent. The service cannot cancel that query,
        // so reclaim a successful late result instead of leaving it in the
        // per-connection registry until eviction or disconnect.
        if (opcode == PipeProtocol.Op.Query && status == PipeProtocol.Status.Ok)
        {
            try
            {
                var (resultId, _, _) = PipeProtocol.DecodeQueryResp(payload);
                ReleaseResult(resultId, CurrentEpoch);
            }
            catch (Exception ex)
            {
                FileLog.Warn("pipe", "late query result could not be released", ex);
            }
        }
    }

    /// <summary>Event pushes fire handlers on the read-loop thread; the
    /// same contract as FFI engine threads — consumers marshal.</summary>
    /// <param name="opcode">Event kind echoed in the frame header.</param>
    /// <param name="payload">Encoded event frame body to decode and dispatch.</param>
    private void DispatchEvent(ushort opcode, byte[] payload)
    {
        var (kind, entries, volume) = PipeProtocol.DecodeEvent(payload);
        if (kind != opcode)
        {
            throw new InvalidDataException(
                $"event opcode/body kind mismatch ({opcode}/{kind})");
        }

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
                throw new InvalidDataException($"unknown event kind {kind}");
        }
    }

    /// <summary>A faulting consumer must not kill the read loop (don't crash).</summary>
    /// <param name="raise">The handler invocation to run guarded.</param>
    /// <param name="what">Label for the event, used in the failure log.</param>
    private static void RaiseSafe(Action raise, string what)
    {
        try
        {
            raise();
        }
        catch (Exception ex)
        {
            FileLog.Error("pipe", "event handler failed", ex);
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
            if (_pending.TryRemove(id, out var pending))
            {
                pending.Completion.TrySetException(
                    new EngineUnavailableException("engine service connection lost"));
            }
        }
    }

    private static void SafeDispose(NamedPipeClientStream? d)
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
    private Task<(int Status, byte[] Payload, int Epoch)> RequestAsync(
        ushort opcode,
        byte[] payload,
        CancellationToken ct = default) =>
        RequestAsyncCore(opcode, payload, null, sendQueryCancel: false, ct);

    private async Task<(int Status, byte[] Payload, int Epoch)> RequestAsyncCore(
        ushort opcode,
        byte[] payload,
        int? expectedEpoch,
        bool sendQueryCancel,
        CancellationToken ct)
    {
        ct.ThrowIfCancellationRequested();

        // Grab the connection object once; from here on it answers its own
        // liveness (a write racing teardown surfaces as
        // EngineUnavailableException inside PipeConnection) — there is no
        // null-check-then-write window against a mutable stream field.
        var conn = Volatile.Read(ref _connection);
        if (conn is null)
        {
            var terminalFailure = Volatile.Read(ref _terminalFailure);
            throw new EngineUnavailableException(terminalFailure is null
                ? "engine service is not connected"
                : $"engine service connection failed permanently: {terminalFailure}");
        }

        if (expectedEpoch is { } epoch && conn.Epoch != epoch)
        {
            throw new StaleResultException();
        }

        var tcs = new TaskCompletionSource<(int Status, byte[] Payload)>(
            TaskCreationOptions.RunContinuationsAsynchronously);
        uint id;
        var pending = new PendingRequest(opcode, tcs);
        do
        {
            id = unchecked((uint)Interlocked.Increment(ref _requestId));
        }
        while (id == 0 || !_pending.TryAdd(id, pending));

        // The caller's ct joins the client-lifetime token: either one aborts
        // the wait. Caller cancellation surfaces as OperationCanceledException;
        // a client-lifetime cancellation (Dispose) keeps reading as
        // EngineUnavailableException, same as before ct plumbing existed.
        using var linked = CancellationTokenSource.CreateLinkedTokenSource(_cts.Token, ct);
        CancellationTokenRegistration queryCancellation = default;

        try
        {
            var frame = PipeProtocol.EncodeFrame(opcode, 0, id, 0, payload);

            // A Query frame must finish before its cancel frame. Caller
            // cancellation therefore does not interrupt this serialized
            // write; registration immediately after it observes a token that
            // cancelled during the write and sends QueryCancel without a
            // lost pre-registration window.
            await conn.WriteFrameAsync(
                frame,
                sendQueryCancel ? _cts.Token : linked.Token).ConfigureAwait(false);
            if (sendQueryCancel)
            {
                queryCancellation = ct.UnsafeRegister(
                    static state =>
                    {
                        var (client, connection, requestId) =
                            ((PipeEngineClient, PipeConnection, uint))state!;
                        client.SendQueryCancel(connection, requestId);
                    },
                    (this, conn, id));
            }

            var (status, responsePayload) = await tcs.Task
                .WaitAsync(RequestTimeout, linked.Token)
                .ConfigureAwait(false);
            return (status, responsePayload, conn.Epoch);
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
            await queryCancellation.DisposeAsync().ConfigureAwait(false);
            _pending.TryRemove(id, out _);
        }
    }

    private void SendQueryCancel(PipeConnection connection, uint requestId)
    {
        if (connection.Epoch != CurrentEpoch || Volatile.Read(ref _disposed) != 0)
        {
            return;
        }

        var frame = PipeProtocol.EncodeFrame(
            PipeProtocol.Op.QueryCancel,
            0,
            requestId,
            0,
            []);
        connection
            .WriteFrameAsync(frame, _cts.Token)
            .Forget("pipe.query-cancel");
    }

    /// <summary>Request + FFI-equivalent status mapping (error responses
    /// carry the detail text inline).</summary>
    /// <param name="opcode">Operation code of the request frame.</param>
    /// <param name="payload">Request body bytes.</param>
    /// <param name="operation">Operation name for the failure message.</param>
    /// <param name="ct">Cancels the request.</param>
    private async Task<byte[]> RequestOkAsync(
        ushort opcode,
        byte[] payload,
        string operation,
        CancellationToken ct = default)
    {
        var (responsePayload, _) = await RequestOkWithEpochAsync(
            opcode,
            payload,
            operation,
            ct).ConfigureAwait(false);
        return responsePayload;
    }

    private async Task<(byte[] Payload, int Epoch)> RequestOkWithEpochAsync(
        ushort opcode,
        byte[] payload,
        string operation,
        CancellationToken ct = default)
    {
        var (status, resp, epoch) = await RequestAsync(opcode, payload, ct).ConfigureAwait(false);
        return (EnsureOk(status, resp, operation), epoch);
    }

    private async Task<byte[]> RequestOkOnEpochAsync(
        ushort opcode,
        byte[] payload,
        string operation,
        int expectedEpoch,
        CancellationToken ct = default)
    {
        var (status, resp, _) = await RequestAsyncCore(
            opcode,
            payload,
            expectedEpoch,
            sendQueryCancel: false,
            ct).ConfigureAwait(false);
        return EnsureOk(status, resp, operation);
    }

    private static byte[] EnsureOk(int status, byte[] payload, string operation) =>
        status == PipeProtocol.Status.Ok
            ? payload
            : throw status switch
            {
                PipeProtocol.Status.QuerySyntax => new QuerySyntaxException(Detail(payload)),
                PipeProtocol.Status.Stale => new StaleResultException(),
                PipeProtocol.Status.Cancelled => new OperationCanceledException(
                    "query cancelled by the engine"),
                _ => new EngineException(
                    $"{operation} failed ({status}): {Detail(payload)}",
                    status),
            };

    private static string Detail(byte[] payload) => Encoding.UTF8.GetString(payload);

    // ── IEngineClient ───────────────────────────────────────────────────

    /// <inheritdoc/>
    public async Task<IReadOnlyList<string>> ListVolumesAsync(CancellationToken ct = default)
    {
        ct.ThrowIfCancellationRequested();
        var payload = await RequestOkAsync(PipeProtocol.Op.ListVolumes, [], "ListVolumes", ct)
            .ConfigureAwait(false);
        return [.. PipeProtocol.DecodeVolumeStatuses(payload).Select(s => s.Label)];
    }

    /// <inheritdoc/>
    public async Task StartIndexingAsync(
        IReadOnlyList<string> volumes, CancellationToken ct = default)
    {
        ct.ThrowIfCancellationRequested();
        await RequestOkAsync(
            PipeProtocol.Op.IndexStart, PipeProtocol.EncodeIndexStartReq(volumes), "IndexStart", ct)
            .ConfigureAwait(false);
    }

    /// <inheritdoc/>
    public async Task<IReadOnlyList<VolumeStatus>> GetStatusAsync(CancellationToken ct = default)
    {
        ct.ThrowIfCancellationRequested();
        var payload = await RequestOkAsync(PipeProtocol.Op.IndexStatus, [], "IndexStatus", ct)
            .ConfigureAwait(false);
        return PipeProtocol.DecodeVolumeStatuses(payload);
    }

    /// <inheritdoc/>
    public async Task<SearchOutcome> SearchAsync(
        string query, SearchOptions options, CancellationToken ct = default) =>
        await SearchAsync(query, options, null, ct).ConfigureAwait(false);

    /// <inheritdoc/>
    public async Task<SearchOutcome> SearchAsync(
        string query,
        SearchOptions options,
        ISearchResult? presentationBasis,
        CancellationToken ct = default)
    {
        ct.ThrowIfCancellationRequested();
        query = EngineRequest.QueryText(query);
        ulong basisId = 0;
        if (presentationBasis is not null)
        {
            if (presentationBasis is not PipeSearchResult pipeBasis)
            {
                throw new ArgumentException(
                    "presentation basis belongs to a different engine transport",
                    nameof(presentationBasis));
            }

            pipeBasis.TryGetPresentationBasis(this, out basisId);
        }

        var (status, response, epoch) = await RequestAsyncCore(
            PipeProtocol.Op.Query,
            PipeProtocol.EncodeQueryReq(options, query, basisId),
            expectedEpoch: null,
            sendQueryCancel: true,
            ct).ConfigureAwait(false);
        var resp = EnsureOk(status, response, "Query");
        var (resultId, count, traceJson) = PipeProtocol.DecodeQueryResp(resp);
        try
        {
            ct.ThrowIfCancellationRequested();
            var signedCount = checked((long)count);
            QueryTraceData? trace = null;
            if (traceJson.Length > 0)
            {
                trace = JsonSerializer.Deserialize<QueryTraceData>(traceJson, EngineJson.SnakeCase);
            }

            // Cross-log correlation (ADR-0037): the engine logs the same `rid`
            // (result handle) for this query, so app.log ↔ engine.log join on it.
            // Skipped for an unchanged idle requery, mirroring the engine.
            if (trace is null || !trace.Unchanged)
            {
                FileLog.Event("query", "query served", ("rid", resultId), ("hits", count));
            }
#pragma warning disable CA2000 // ownership transferred to the caller, disposed by the caller / on epoch change
            return new SearchOutcome(
                new PipeSearchResult(this, resultId, signedCount, epoch), trace);
#pragma warning restore CA2000
        }
        catch
        {
            // Once QueryResp has yielded a result id, every exceptional exit
            // still owns that server-side handle. Release it on the same
            // connection epoch; a concurrent disconnect already reclaimed it.
            await ReleaseResultIfCurrentAsync(resultId, epoch).ConfigureAwait(false);
            throw;
        }
    }

    /// <inheritdoc/>
    public async Task<EngineStatsData?> GetStatsAsync(CancellationToken ct = default)
    {
        ct.ThrowIfCancellationRequested();
        byte[] payload;
        try
        {
            int status;
            (status, payload, _) = await RequestAsync(PipeProtocol.Op.Stats, [], ct)
                .ConfigureAwait(false);
            if (status != PipeProtocol.Status.Ok)
            {
                return null; // FFI parity: stats are best-effort
            }
        }
        catch (EngineUnavailableException ex)
        {
            FileLog.Warn("pipe", "stats unavailable", ex);
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

            // Service runtime info is a separate op and a nice-to-have on top of
            // the stats snapshot — best-effort, so a failure here never voids the
            // stats the panel already has.
            stats.Service = await TryGetServiceInfoAsync(ct).ConfigureAwait(false);
        }

        return stats;
    }

    private async Task<ServiceInfoData?> TryGetServiceInfoAsync(CancellationToken ct)
    {
        try
        {
            var (status, payload, _) = await RequestAsync(PipeProtocol.Op.ServiceInfo, [], ct)
                .ConfigureAwait(false);
            return status == PipeProtocol.Status.Ok
                ? JsonSerializer.Deserialize<ServiceInfoData>(payload, EngineJson.SnakeCase)
                : null;
        }
        catch (EngineUnavailableException ex)
        {
            FileLog.Warn("pipe", "service info unavailable", ex);
            return null;
        }
    }

    // ── Result paging (used by PipeSearchResult) ────────────────────────

    /// <summary>Epoch of the live connection; 0 (never a connection's value)
    /// while disconnected. A result is current iff its birth epoch equals
    /// this — connection generations are never reused, so a result born on a
    /// dead connection can never read as current again.</summary>
    internal int CurrentEpoch => Volatile.Read(ref _connection)?.Epoch ?? 0;

    internal async Task<IReadOnlyList<RowData>> FetchPageAsync(
        ulong resultId,
        EngineRequest.Page request,
        int epoch,
        CancellationToken ct)
    {
        ct.ThrowIfCancellationRequested();
        var start = Stopwatch.GetTimestamp();
        var payload = await RequestOkOnEpochAsync(
            PipeProtocol.Op.ResultPage,
            PipeProtocol.EncodeResultPageReq(resultId, request.Offset, request.Count),
            "ResultPage",
            epoch,
            ct).ConfigureAwait(false);
        var rttUs = Stopwatch.GetElapsedTime(start).TotalMicroseconds;
        lock (_statsLock)
        {
            _pageRttEwmaUs = _pageRttEwmaUs == 0 ? rttUs : (0.8 * _pageRttEwmaUs) + (0.2 * rttUs);
        }

        return PipeProtocol.DecodePageResp(payload);
    }

    internal void ReleaseResult(ulong resultId, int epoch)
    {
        ReleaseResultIfCurrentAsync(resultId, epoch).Forget("pipe.release");
    }

    private async Task ReleaseResultIfCurrentAsync(ulong resultId, int epoch)
    {
        if (Volatile.Read(ref _connection) is not { } conn || conn.Epoch != epoch)
        {
            return; // the server freed it together with the dead connection
        }

        try
        {
            await ReleaseResultAsync(resultId, epoch).ConfigureAwait(false);
        }
        catch (StaleResultException)
        {
            // The connection changed after the check. Its result registry
            // died with it; never forward the old handle to the new epoch.
        }
        catch (Exception ex)
        {
            // Cleanup failure must not mask the query/decode exception that
            // made this path necessary.
            FileLog.Warn("pipe", "result release failed", ex);
        }
    }

    private async Task ReleaseResultAsync(ulong resultId, int epoch)
    {
        try
        {
            await RequestOkOnEpochAsync(
                PipeProtocol.Op.ResultFree,
                PipeProtocol.EncodeResultFreeReq(resultId),
                "ResultFree",
                epoch)
                .ConfigureAwait(false);
        }
        catch (EngineUnavailableException)
        {
            // Disconnected mid-release: the per-connection registry on the
            // server already freed it. Not an error worth surfacing.
        }
    }

    /// <inheritdoc/>
    public void Dispose()
    {
        if (Interlocked.Exchange(ref _disposed, 1) != 0)
        {
            return;
        }

        // Stop the supervisor and break the connection; never block shutdown
        // on the background task.
        _cts.Cancel();
        TearDownConnection();

        // The supervisor may still observe the token after we return, so the
        // CTS is disposed only once that background task has actually exited
        // (or immediately if it never started) — never on the Dispose thread.
        var supervisor = _supervisor;
        if (supervisor is null)
        {
            _cts.Dispose();
        }
        else
        {
            supervisor.ContinueWith(
                static (_, state) => ((CancellationTokenSource)state!).Dispose(),
                _cts,
                CancellationToken.None,
                TaskContinuationOptions.ExecuteSynchronously,
                TaskScheduler.Default).Forget("pipe.dispose");
        }
    }

    /// <summary>Conditions a reconnect can never cure — the supervisor stops
    /// for good and every request fails with EngineUnavailableException.</summary>
    private class FatalPipeException(string message) : Exception(message);

    private sealed class ProtocolMismatchException(string message) : FatalPipeException(message);

    private sealed class ServerIdentityException(string message) : FatalPipeException(message);
}
