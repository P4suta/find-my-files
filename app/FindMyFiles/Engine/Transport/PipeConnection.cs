using System.IO.Pipes;
using FindMyFiles.Services;

namespace FindMyFiles.Engine;

/// <summary>Owns one connection generation and normalizes writes that race its teardown.</summary>
internal sealed class PipeConnection(
    NamedPipeClientStream stream,
    int epoch,
    Action<int, ushort, byte[]> onEvent,
    Action<int, uint, ushort, int, byte[]> onResponse,
    CancellationToken ct) : IDisposable
{
    private readonly NamedPipeClientStream _stream = stream;

    [System.Diagnostics.CodeAnalysis.SuppressMessage("Reliability", "CA2213:Disposable fields should be disposed", Justification = "SemaphoreSlim allocates no handle unless AvailableWaitHandle is used (it is not); disposing it breaks write-after-dispose normalization")]
    private readonly SemaphoreSlim _writeLock = new(1, 1);
    private volatile bool _disposed;

    /// <summary>Identifies results from this connection; reconnecting assigns a new generation.</summary>
    internal int Epoch { get; } = epoch;

    /// <summary>Completes when the connection ends so the supervisor can reconnect.
    /// Never faults; failures are logged.</summary>
    internal Task ReadLoop { get; } = Task.Run(
        () => ReadLoopAsync(stream, epoch, onEvent, onResponse, ct),
        CancellationToken.None);

    internal async Task WriteFrameAsync(byte[] frame, CancellationToken ct)
    {
        await _writeLock.WaitAsync(ct).ConfigureAwait(ConfigureAwaitOptions.None);
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

    private static async Task ReadLoopAsync(
        NamedPipeClientStream stream,
        int epoch,
        Action<int, ushort, byte[]> onEvent,
        Action<int, uint, ushort, int, byte[]> onResponse,
        CancellationToken ct)
    {
        try
        {
            for (; ;)
            {
                var h = await ReadHeaderAsync(stream, ct).ConfigureAwait(false);
                var payload = new byte[h.Len];
                await stream.ReadExactlyAsync(payload, ct).ConfigureAwait(false);

                switch (h.Flags)
                {
                    case PipeProtocol.FlagEvent
                        when h.RequestId == 0 && h.StatusCode == PipeProtocol.Status.Ok:
                        onEvent(epoch, h.Opcode, payload);
                        break;
                    case PipeProtocol.FlagEvent:
                        throw new InvalidDataException(
                            $"event frame has request/status fields "
                            + $"({h.RequestId}/{h.StatusCode})");
                    case PipeProtocol.FlagResponse when h.RequestId != 0:
                        onResponse(epoch, h.RequestId, h.Opcode, h.StatusCode, payload);
                        break;
                    case PipeProtocol.FlagResponse:
                        throw new InvalidDataException("response frame has request id 0");
                    default:
                        throw new InvalidDataException(
                            $"frame has invalid flags 0x{h.Flags:X4} (opcode {h.Opcode})");
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
            FileLog.Warn("pipe", "read loop ended", ex);
        }
    }

    private static async Task<PipeProtocol.FrameHeader> ReadHeaderAsync(
        NamedPipeClientStream stream,
        CancellationToken ct)
    {
        var header = new byte[PipeProtocol.HeaderLen];
        var bytesRead = await stream.ReadAtLeastAsync(
            header,
            header.Length,
            throwOnEndOfStream: true,
            ct).ConfigureAwait(false);
        return PipeProtocol.ReadHeader(header.AsSpan(0, bytesRead));
    }

    /// <summary>Invalidates the whole connection.
    /// Idempotent; an in-flight write surfaces as EngineUnavailableException via the normalization in <see cref="WriteFrameAsync"/>.</summary>
    public void Dispose()
    {
        _disposed = true;
        try
        {
            _stream.Dispose();
        }
        catch
        {
        }
    }
}
