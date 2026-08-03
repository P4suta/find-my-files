using System.Buffers.Binary;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.Json;

namespace FindMyFiles.Engine;

/// <summary>
/// Wire codec for the fmf-service named pipe: 16-byte LE frame header +
/// length-prefixed payload, binary hot path, JSON cold path. Pure functions
/// and constants only — the canonical wire definition is the fmf-contract
/// crate radiated into <see cref="EngineContract"/> (ADR-0018), and the Rust
/// twin (fmf-proto) pins byte-identical golden frames from contract/golden/.
/// </summary>
internal static class PipeProtocol
{
    // All values radiate from Generated/EngineContract.g.cs (ADR-0018);
    // these aliases keep the historical spelling at the call sites.
    public const uint ProtocolVersion = EngineContract.ProtocolVersion;

    /// <summary>Short pipe name (without the <c>\\.\pipe\</c> prefix).</summary>
    public const string DefaultPipeName = EngineContract.PipeNameShort;

    public const int HeaderLen = EngineContract.FrameHeaderLen;
    public const uint MaxPayloadLen = EngineContract.MaxPayloadLen;
    public const ushort FlagResponse = 1 << 0;
    public const ushort FlagEvent = 1 << 1;
    public const int RowSize = EngineContract.RowSize;

    public static class Op
    {
        public const ushort Hello = EngineContract.Op.Hello;
        public const ushort Subscribe = EngineContract.Op.Subscribe;
        public const ushort Unsubscribe = EngineContract.Op.Unsubscribe;
        public const ushort ListVolumes = EngineContract.Op.ListVolumes;
        public const ushort IndexStart = EngineContract.Op.IndexStart;
        public const ushort IndexStatus = EngineContract.Op.IndexStatus;
        public const ushort Query = EngineContract.Op.Query;
        public const ushort ResultPage = EngineContract.Op.ResultPage;
        public const ushort ResultFree = EngineContract.Op.ResultFree;
        public const ushort Stats = EngineContract.Op.Stats;
        public const ushort ServiceInfo = EngineContract.Op.ServiceInfo;
        public const ushort QueryCancel = EngineContract.Op.QueryCancel;
    }

    /// <summary>Status codes — the FFI error table verbatim (shared).</summary>
    public static class Status
    {
        public const int Ok = EngineContract.Status.Ok;
        public const int InvalidArg = EngineContract.Status.InvalidArg;
        public const int Stale = EngineContract.Status.Stale;
        public const int NotAdmin = EngineContract.Status.NotAdmin;
        public const int Volume = EngineContract.Status.Volume;
        public const int QuerySyntax = EngineContract.Status.QuerySyntax;
        public const int Io = EngineContract.Status.Io;
        public const int Locked = EngineContract.Status.Locked;
        public const int Cancelled = EngineContract.Status.Cancelled;
        public const int Panic = EngineContract.Status.Panic;
    }

    [StructLayout(LayoutKind.Auto)]
    public readonly record struct FrameHeader(
        uint Len, ushort Opcode, ushort Flags, uint RequestId, int StatusCode)
    {
        public bool IsResponse => (Flags & FlagResponse) != 0;

        public bool IsEvent => (Flags & FlagEvent) != 0;
    }

    // ── Frame header ────────────────────────────────────────────────────
    public static void WriteHeader(Span<byte> dest, FrameHeader h)
    {
        BinaryPrimitives.WriteUInt32LittleEndian(dest, h.Len);
        BinaryPrimitives.WriteUInt16LittleEndian(dest[4..], h.Opcode);
        BinaryPrimitives.WriteUInt16LittleEndian(dest[6..], h.Flags);
        BinaryPrimitives.WriteUInt32LittleEndian(dest[8..], h.RequestId);
        BinaryPrimitives.WriteInt32LittleEndian(dest[12..], h.StatusCode);
    }

    /// <summary>Decodes a <see cref="FrameHeader"/> from the first bytes of a
    /// frame on the wire (the inverse of the header writer above).</summary>
    /// <param name="src">The frame bytes; the first <see cref="HeaderLen"/> are read.</param>
    /// <exception cref="InvalidDataException">announced payload over the cap
    /// — the connection has no resync point and must be dropped</exception>
    /// <returns>The decoded frame header.</returns>
    public static FrameHeader ReadHeader(ReadOnlySpan<byte> src)
    {
        var h = new FrameHeader(
            BinaryPrimitives.ReadUInt32LittleEndian(src),
            BinaryPrimitives.ReadUInt16LittleEndian(src[4..]),
            BinaryPrimitives.ReadUInt16LittleEndian(src[6..]),
            BinaryPrimitives.ReadUInt32LittleEndian(src[8..]),
            BinaryPrimitives.ReadInt32LittleEndian(src[12..]));
        if (h.Len > MaxPayloadLen)
        {
            throw new InvalidDataException(
                $"frame payload {h.Len} bytes exceeds the {MaxPayloadLen}-byte cap");
        }

        return h;
    }

    /// <summary>One contiguous frame: header (len filled in) + payload.</summary>
    /// <param name="opcode">Operation code for the frame.</param>
    /// <param name="flags">Frame flag bits (response/event).</param>
    /// <param name="requestId">Request correlation id; 0 for events.</param>
    /// <param name="status">Status code carried in the header.</param>
    /// <param name="payload">Payload bytes to append after the header.</param>
    /// <returns>The header followed by the payload, as one byte array.</returns>
    public static byte[] EncodeFrame(
        ushort opcode, ushort flags, uint requestId, int status, ReadOnlySpan<byte> payload)
    {
        var buf = new byte[HeaderLen + payload.Length];
        WriteHeader(buf, new FrameHeader((uint)payload.Length, opcode, flags, requestId, status));
        payload.CopyTo(buf.AsSpan(HeaderLen));
        return buf;
    }

    // ── Hello (op 1, binary) ────────────────────────────────────────────
    public static byte[] EncodeHelloReq(uint protocolVersion)
    {
        var b = new byte[4];
        BinaryPrimitives.WriteUInt32LittleEndian(b, protocolVersion);
        return b;
    }

    public static byte[] EncodeHelloResp(uint protocolVersion, uint abiVersion, uint serverPid)
    {
        var b = new byte[12];
        BinaryPrimitives.WriteUInt32LittleEndian(b, protocolVersion);
        BinaryPrimitives.WriteUInt32LittleEndian(b.AsSpan(4), abiVersion);
        BinaryPrimitives.WriteUInt32LittleEndian(b.AsSpan(8), serverPid);
        return b;
    }

    public static (uint ProtocolVersion, uint AbiVersion, uint ServerPid) DecodeHelloResp(
        ReadOnlySpan<byte> payload)
    {
        CheckLen("HelloResp", payload, 12);
        return (
            BinaryPrimitives.ReadUInt32LittleEndian(payload),
            BinaryPrimitives.ReadUInt32LittleEndian(payload[4..]),
            BinaryPrimitives.ReadUInt32LittleEndian(payload[8..]));
    }

    // ── Query (op 7, 32B POD options + UTF-8 text) ──────────────────────
    public static byte[] EncodeQueryReq(
        SearchOptions options,
        string text,
        ulong presentationBasis = 0)
    {
        text = EngineRequest.QueryText(text);

        // Size the frame off the UTF-8 byte count and encode the text straight
        // into it — no intermediate array + copy (this runs per keystroke).
        var b = new byte[EngineContract.QueryOptionsSize + Encoding.UTF8.GetByteCount(text)];
        BinaryPrimitives.WriteUInt32LittleEndian(b, (uint)options.Sort);
        BinaryPrimitives.WriteUInt32LittleEndian(b.AsSpan(4), options.Descending ? 1u : 0u);
        BinaryPrimitives.WriteUInt32LittleEndian(b.AsSpan(8), (uint)options.Case);
        BinaryPrimitives.WriteUInt32LittleEndian(
            b.AsSpan(12), options.IncludeHiddenSystem ? 1u : 0u);
        BinaryPrimitives.WriteUInt32LittleEndian(b.AsSpan(16), options.RegexModeBits);
        BinaryPrimitives.WriteUInt64LittleEndian(b.AsSpan(24), presentationBasis);
        Encoding.UTF8.GetBytes(text, b.AsSpan(EngineContract.QueryOptionsSize));
        return b;
    }

    public static (SearchOptions Options, string Text, ulong PresentationBasis) DecodeQueryReq(
        ReadOnlySpan<byte> payload)
    {
        var len = EngineContract.QueryOptionsSize;
        if (payload.Length < len)
        {
            throw new InvalidDataException(
                $"QueryReq payload is {payload.Length} bytes, need ≥{len}");
        }

        var regexBits = BinaryPrimitives.ReadUInt32LittleEndian(payload[16..]);
        if (BinaryPrimitives.ReadUInt32LittleEndian(payload[20..]) != 0)
        {
            throw new InvalidDataException("QueryReq reserved field is nonzero");
        }

        var options = new SearchOptions(
            (FmfSort)BinaryPrimitives.ReadUInt32LittleEndian(payload),
            BinaryPrimitives.ReadUInt32LittleEndian(payload[4..]) != 0,
            (FmfCase)BinaryPrimitives.ReadUInt32LittleEndian(payload[8..]),
            BinaryPrimitives.ReadUInt32LittleEndian(payload[12..]) != 0,
            (regexBits & 1u) != 0,
            (regexBits & 2u) != 0 ? RegexScope.Path : RegexScope.Name);
        return (
            options,
            Encoding.UTF8.GetString(payload[len..]),
            BinaryPrimitives.ReadUInt64LittleEndian(payload[24..]));
    }

    public static byte[] EncodeQueryResp(ulong resultId, ulong count, string traceJson)
    {
        var traceBytes = Encoding.UTF8.GetBytes(traceJson);
        var b = new byte[16 + traceBytes.Length];
        BinaryPrimitives.WriteUInt64LittleEndian(b, resultId);
        BinaryPrimitives.WriteUInt64LittleEndian(b.AsSpan(8), count);
        traceBytes.CopyTo(b, 16);
        return b;
    }

    public static (ulong ResultId, ulong Count, string TraceJson) DecodeQueryResp(
        ReadOnlySpan<byte> payload)
    {
        if (payload.Length < 16)
        {
            throw new InvalidDataException($"QueryResp payload is {payload.Length} bytes, need ≥16");
        }

        return (
            BinaryPrimitives.ReadUInt64LittleEndian(payload),
            BinaryPrimitives.ReadUInt64LittleEndian(payload[8..]),
            Encoding.UTF8.GetString(payload[16..]));
    }

    // ── ResultPage (op 8, binary) ───────────────────────────────────────
    public static byte[] EncodeResultPageReq(ulong resultId, ulong offset, uint count)
    {
        var b = new byte[20];
        BinaryPrimitives.WriteUInt64LittleEndian(b, resultId);
        BinaryPrimitives.WriteUInt64LittleEndian(b.AsSpan(8), offset);
        BinaryPrimitives.WriteUInt32LittleEndian(b.AsSpan(16), count);
        return b;
    }

    public static (ulong ResultId, ulong Offset, uint Count) DecodeResultPageReq(
        ReadOnlySpan<byte> payload)
    {
        CheckLen("ResultPageReq", payload, 20);
        return (
            BinaryPrimitives.ReadUInt64LittleEndian(payload),
            BinaryPrimitives.ReadUInt64LittleEndian(payload[8..]),
            BinaryPrimitives.ReadUInt32LittleEndian(payload[16..]));
    }

    /// <summary>`{row_count:u32, blob_len:u32}` + 56B rows + WTF-8 blob.</summary>
    /// <param name="payload">The ResultPage response payload bytes.</param>
    /// <returns>The decoded rows from the page.</returns>
    public static List<RowData> DecodePageResp(ReadOnlySpan<byte> payload)
    {
        if (payload.Length < 8)
        {
            throw new InvalidDataException($"PageResp payload is {payload.Length} bytes, need ≥8");
        }

        var rowCount = BinaryPrimitives.ReadUInt32LittleEndian(payload);
        var blobLen = BinaryPrimitives.ReadUInt32LittleEndian(payload[4..]);
        if (rowCount > (uint)EngineContract.MaxPageRows)
        {
            throw new InvalidDataException(
                $"PageResp row_count {rowCount} exceeds {EngineContract.MaxPageRows}");
        }

        var validatedPayloadLength = ValidateDecodedPagePayloadLength(payload.Length);

        // Validate the declared sizes in long: the fields are u32, so
        // `rowCount * RowSize` overflows int for a hostile/buggy frame. The 16 MiB
        // frame cap already bounds payload.Length, so once this equality holds
        // every offset below fits an int.
        var expected = 8L + ((long)rowCount * RowSize) + blobLen;
        if (expected != validatedPayloadLength)
        {
            throw new InvalidDataException(
                $"PageResp payload is {payload.Length} bytes, expected {expected} for {rowCount} rows");
        }

        var rowBytes = (int)rowCount * RowSize;
        return PageCodec.Decode(
            payload.Slice(8, rowBytes),
            payload.Slice(8 + rowBytes, (int)blobLen));
    }

    public static byte[] EncodePageResp(IReadOnlyList<RowData> rows)
    {
        ArgumentNullException.ThrowIfNull(rows);
        if (rows.Count > EngineContract.MaxPageRows)
        {
            throw new ArgumentOutOfRangeException(
                nameof(rows),
                rows.Count,
                $"A page may contain at most {EngineContract.MaxPageRows} rows.");
        }

        var encoded = new (byte[] Name, byte[] Parent)[rows.Count];
        long blobLength = 0;
        for (var i = 0; i < rows.Count; i++)
        {
            encoded[i] = (Wtf8.Encode(rows[i].Name), Wtf8.Encode(rows[i].ParentPath));
            blobLength += encoded[i].Name.Length + encoded[i].Parent.Length;
        }

        // rows.Count is contract-bounded above and every allocation length is
        // int-sized. The payload cap below therefore proves each later cast.
        var rowBytesLength = rows.Count * RowSize;
        var payloadLength = 8L + rowBytesLength + blobLength;
        var validatedPayloadLength = ValidateEncodedPagePayloadLength(payloadLength);

        var blob = new byte[(int)blobLength];
        var blobOffset = 0;
        var rowBytes = new byte[rowBytesLength];
        for (var i = 0; i < rows.Count; i++)
        {
            var row = rows[i];
            var (name, parent) = encoded[i];
            var nameOff = (uint)blobOffset;
            name.CopyTo(blob, blobOffset);
            blobOffset += name.Length;
            var parentOff = (uint)blobOffset;
            parent.CopyTo(blob, blobOffset);
            blobOffset += parent.Length;

            var r = rowBytes.AsSpan(i * RowSize, RowSize);
            BinaryPrimitives.WriteUInt64LittleEndian(
                r[EngineContract.RowOffsets.EntryRef..], row.EntryRef);
            BinaryPrimitives.WriteUInt64LittleEndian(r[EngineContract.RowOffsets.Frn..], row.Frn);
            BinaryPrimitives.WriteUInt64LittleEndian(r[EngineContract.RowOffsets.Size..], row.Size);
            BinaryPrimitives.WriteInt64LittleEndian(r[EngineContract.RowOffsets.Mtime..], row.Mtime);
            BinaryPrimitives.WriteUInt32LittleEndian(r[EngineContract.RowOffsets.NameOff..], nameOff);
            BinaryPrimitives.WriteUInt32LittleEndian(
                r[EngineContract.RowOffsets.ParentPathOff..], parentOff);
            BinaryPrimitives.WriteUInt32LittleEndian(r[EngineContract.RowOffsets.Flags..], row.Flags);
            BinaryPrimitives.WriteUInt32LittleEndian(
                r[EngineContract.RowOffsets.NameLen..], (uint)name.Length);
            BinaryPrimitives.WriteUInt32LittleEndian(
                r[EngineContract.RowOffsets.ParentPathLen..], (uint)parent.Length);
        }

        var b = new byte[validatedPayloadLength];
        BinaryPrimitives.WriteUInt32LittleEndian(b, (uint)rows.Count);
        BinaryPrimitives.WriteUInt32LittleEndian(b.AsSpan(4), (uint)blob.Length);
        rowBytes.CopyTo(b, 8);
        blob.CopyTo(b, 8 + rowBytes.Length);
        return b;
    }

    /// <summary>Validate a received page length without allocating that page.</summary>
    /// <param name="payloadLength">Received payload byte length.</param>
    /// <returns>The validated length for subsequent size equations.</returns>
    internal static int ValidateDecodedPagePayloadLength(int payloadLength)
    {
        if ((ulong)payloadLength > EngineContract.MaxPayloadLen)
        {
            throw new InvalidDataException(
                $"PageResp payload is {payloadLength} bytes, maximum is {EngineContract.MaxPayloadLen}");
        }

        return payloadLength;
    }

    /// <summary>Validate a page encoder's calculated length without allocating it.</summary>
    /// <param name="payloadLength">Calculated payload byte length.</param>
    /// <returns>The validated int-sized allocation length.</returns>
    internal static int ValidateEncodedPagePayloadLength(long payloadLength)
    {
        if (payloadLength > EngineContract.MaxPayloadLen)
        {
            throw new ArgumentOutOfRangeException(
                nameof(payloadLength),
                payloadLength,
                $"Encoded page is {payloadLength} bytes; maximum is {EngineContract.MaxPayloadLen}.");
        }

        return (int)payloadLength;
    }

    // ── ResultFree (op 9, binary) ───────────────────────────────────────
    public static byte[] EncodeResultFreeReq(ulong resultId)
    {
        var b = new byte[8];
        BinaryPrimitives.WriteUInt64LittleEndian(b, resultId);
        return b;
    }

    public static ulong DecodeResultFreeReq(ReadOnlySpan<byte> payload)
    {
        CheckLen("ResultFreeReq", payload, 8);
        return BinaryPrimitives.ReadUInt64LittleEndian(payload);
    }

    // ── Event push (flags bit1, request_id=0, opcode = kind 1..6) ───────

    /// <summary>32B POD `{kind:u32, _pad:u32, entries:u64, volume:[u8;16]}`;
    /// volume is the zero-padded UTF-8 drive label ("C:"), not a GUID.</summary>
    /// <param name="payload">The 32-byte event payload bytes.</param>
    /// <returns>The event kind, entry count, and source volume label.</returns>
    public static (uint Kind, ulong Entries, string Volume) DecodeEvent(ReadOnlySpan<byte> payload)
    {
        CheckLen("Event", payload, 32);
        var volume = payload.Slice(16, 16);
        var len = volume.IndexOf((byte)0);
        if (len < 0)
        {
            len = 16;
        }

        return (
            BinaryPrimitives.ReadUInt32LittleEndian(payload),
            BinaryPrimitives.ReadUInt64LittleEndian(payload[8..]),
            Encoding.UTF8.GetString(volume[..len]));
    }

    public static byte[] EncodeEvent(uint kind, ulong entries, string volume)
    {
        var b = new byte[32];
        BinaryPrimitives.WriteUInt32LittleEndian(b, kind);
        BinaryPrimitives.WriteUInt64LittleEndian(b.AsSpan(8), entries);
        var label = Encoding.UTF8.GetBytes(volume);
        label.AsSpan(0, Math.Min(label.Length, 15)).CopyTo(b.AsSpan(16));
        return b;
    }

    // ── JSON payloads (op 4/5/6/10/12, snake_case via EngineJson) ───────
    // internal (not private) only so EngineJsonContext can register them for
    // source-generated (de)serialization; they remain engine-internal DTOs.
    internal sealed class VolumeStatusJson
    {
        public string Volume { get; set; } = string.Empty;

        public uint State { get; set; }

        public ulong Entries { get; set; }
    }

    internal sealed class IndexStartJson
    {
        public List<string> Volumes { get; set; } = [];
    }

    /// <summary>`[{"volume":"C:","state":1,"entries":42}]` — ListVolumes and
    /// IndexStatus share this shape; state values equal VolumeState.</summary>
    /// <param name="payload">The JSON volume-status array as UTF-8 bytes.</param>
    /// <returns>The decoded per-volume status list.</returns>
    public static List<VolumeStatus> DecodeVolumeStatuses(ReadOnlySpan<byte> payload)
    {
        var wire = JsonSerializer.Deserialize<List<VolumeStatusJson>>(payload, EngineJson.SnakeCase) ?? [];
        return [.. wire.Select(w => new VolumeStatus(w.Volume, (VolumeState)w.State, w.Entries))];
    }

    public static byte[] EncodeVolumeStatuses(IEnumerable<VolumeStatus> statuses)
    {
        var wire = statuses
            .Select(s => new VolumeStatusJson
            {
                Volume = s.Label,
                State = (uint)s.State,
                Entries = s.Entries,
            })
            .ToList();
        return JsonSerializer.SerializeToUtf8Bytes(wire, EngineJson.SnakeCase);
    }

    public static byte[] EncodeIndexStartReq(IReadOnlyList<string> volumes)
    {
        var snapshot = EngineRequest.Volumes(volumes);
        return JsonSerializer.SerializeToUtf8Bytes(
            new IndexStartJson { Volumes = [.. snapshot] },
            EngineJson.SnakeCase);
    }

    public static IReadOnlyList<string> DecodeIndexStartReq(ReadOnlySpan<byte> payload) =>
        (JsonSerializer.Deserialize<IndexStartJson>(payload, EngineJson.SnakeCase) ?? new()).Volumes;

    private static void CheckLen(string what, ReadOnlySpan<byte> payload, int expected)
    {
        if (payload.Length != expected)
        {
            throw new InvalidDataException(
                $"{what} payload is {payload.Length} bytes, expected {expected}");
        }
    }
}
