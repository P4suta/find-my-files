using System.IO.Pipes;
using FindMyFiles.Services;

namespace FindMyFiles.Engine;

/// <summary>
/// One established pipe connection, owned whole: the stream, the read loop,
/// the serialized writer and the connection generation (<see cref="Epoch"/>)
/// live and die together. Disconnection = invalidating this object — a write
/// that races teardown is answered by the object itself (normalized to
/// <see cref="EngineUnavailableException"/> inside
/// <see cref="WriteFrameAsync"/>), so callers hold a reference that knows its
/// own liveness instead of a "volatile null-check, then write" convention
/// against a mutable stream field. <see cref="PipeEngineClient"/> supervises:
/// it creates one of these per (re)connection and replaces it wholesale.
/// </summary>
internal sealed class PipeConnection : IDisposable
{
    private readonly NamedPipeClientStream _stream;
    // CA2213 false positive: SemaphoreSlim only owns a disposable handle if its
    // AvailableWaitHandle is accessed — it never is here (only WaitAsync/Release).
    // Disposing it would also break the write-after-dispose normalization below
    // (a disposed semaphore throws ObjectDisposedException from WaitAsync, bypassing
    // the EngineUnavailableException translation PipeConnectionTests pins).
    [System.Diagnostics.CodeAnalysis.SuppressMessage("Reliability", "CA2213:Disposable fields should be disposed", Justification = "SemaphoreSlim allocates no handle unless AvailableWaitHandle is used (it is not); disposing it breaks write-after-dispose normalization")]
    private readonly SemaphoreSlim _writeLock = new(1, 1);
    private readonly Action<byte[]> _onEvent;
    private readonly Action<uint, int, byte[]> _onResponse;
    private volatile bool _disposed;

    /// <summary>Generation id of this connection (monotonic, never reused).
    /// Results born on this connection carry it; once the object dies the
    /// client's CurrentEpoch can never equal it again, so epoch mismatch ⇒
    /// stale holds structurally, not by convention.</summary>
    internal int Epoch { get; }

    /// <summary>Completes when the connection is dead (server closed the
    /// pipe, malformed frame, cancellation) — the supervisor awaits this to
    /// tear down and reconnect. Never faults; failures are logged.</summary>
    internal Task ReadLoop { get; }

    internal PipeConnection(
        NamedPipeClientStream stream,
        int epoch,
        Action<byte[]> onEvent,
        Action<uint, int, byte[]> onResponse,
        CancellationToken ct)
    {
        _stream = stream;
        Epoch = epoch;
        _onEvent = onEvent;
        _onResponse = onResponse;
        ReadLoop = Task.Run(() => ReadLoopAsync(ct), CancellationToken.None);
    }

    /// <summary>Serialized frame write. A write that lands after
    /// <see cref="Dispose"/> (the supervisor tore this connection down) is
    /// normalized to <see cref="EngineUnavailableException"/> right here, in
    /// the owner of the stream — the structural replacement for catching
    /// ObjectDisposedException at every call site.</summary>
    internal async Task WriteFrameAsync(byte[] frame, CancellationToken ct)
    {
        await _writeLock.WaitAsync(ct).ConfigureAwait(false);
        try
        {
            if (_disposed)
            {
                throw new EngineUnavailableException(
                    "engine service connection lost: connection is closed");
            }
            await _stream.WriteAsync(frame, ct).ConfigureAwait(false);
        }
        catch (Exception ex) when (ex is IOException or ObjectDisposedException)
        {
            throw new EngineUnavailableException(
                $"engine service connection lost: {ex.Message}");
        }
        finally
        {
            _writeLock.Release();
        }
    }

    private async Task ReadLoopAsync(CancellationToken ct)
    {
        var header = new byte[PipeProtocol.HeaderLen];
        try
        {
            while (!ct.IsCancellationRequested)
            {
                await _stream.ReadExactlyAsync(header, ct).ConfigureAwait(false);
                var h = PipeProtocol.ReadHeader(header); // oversize throws → drop the link
                var payload = new byte[h.Len];
                if (h.Len > 0)
                {
                    await _stream.ReadExactlyAsync(payload, ct).ConfigureAwait(false);
                }
                if (h.IsEvent)
                {
                    _onEvent(payload);
                }
                else if (h.IsResponse)
                {
                    _onResponse(h.RequestId, h.StatusCode, payload);
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

    /// <summary>Invalidates the whole connection. Idempotent; an in-flight
    /// write surfaces as EngineUnavailableException via the normalization in
    /// <see cref="WriteFrameAsync"/>.</summary>
    public void Dispose()
    {
        _disposed = true;
        try
        {
            _stream.Dispose();
        }
        catch
        {
            // Already broken — nothing to report.
        }
    }
}
