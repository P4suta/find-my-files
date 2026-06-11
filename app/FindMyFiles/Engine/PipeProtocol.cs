using System.Buffers.Binary;
using System.Text;
using System.Text.Json;

namespace FindMyFiles.Engine;

/// <summary>
/// Wire codec for the fmf-service named pipe: 16-byte LE frame header +
/// length-prefixed payload, binary hot path, JSON cold path. Pure functions
/// and constants only — docs/ARCHITECTURE.md「Pipe プロトコル」 is canonical,
/// and the Rust twin (fmf-proto) pins identical golden bytes.
/// </summary>
internal static class PipeProtocol
{
    public const uint ProtocolVersion = 1;

    /// <summary>Short pipe name (without the <c>\\.\pipe\</c> prefix).</summary>
    public const string DefaultPipeName = "fmf-engine-v1";

    public const int HeaderLen = 16;
    public const uint MaxPayloadLen = 16 * 1024 * 1024;
    public const ushort FlagResponse = 1 << 0;
    public const ushort FlagEvent = 1 << 1;
    public const int RowSize = PageCodec.RowSize;

    public static class Op
    {
        public const ushort Hello = 1;
        public const ushort Subscribe = 2;
        public const ushort Unsubscribe = 3;
        public const ushort ListVolumes = 4;
        public const ushort IndexStart = 5;
        public const ushort IndexStatus = 6;
        public const ushort Query = 7;
        public const ushort ResultPage = 8;
        public const ushort ResultFree = 9;
        public const ushort Stats = 10;
        public const ushort ServiceInfo = 12;
    }

    /// <summary>Status codes — the FFI error table verbatim (shared).</summary>
    public static class Status
    {
        public const int Ok = 0;
        public const int InvalidArg = 1;
        public const int Stale = 2;
        public const int NotAdmin = 3;
        public const int Volume = 4;
        public const int QuerySyntax = 5;
        public const int Io = 6;
        public const int Locked = 7;
        public const int Panic = 99;
    }

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

    /// <exception cref="InvalidDataException">announced payload over the cap
    /// — the connection has no resync point and must be dropped</exception>
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

    // ── Query (op 7, 16B POD options + UTF-8 text) ──────────────────────

    public static byte[] EncodeQueryReq(SearchOptions options, string text)
    {
        var textBytes = Encoding.UTF8.GetBytes(text);
        var b = new byte[16 + textBytes.Length];
        BinaryPrimitives.WriteUInt32LittleEndian(b, (uint)options.Sort);
        BinaryPrimitives.WriteUInt32LittleEndian(b.AsSpan(4), options.Descending ? 1u : 0u);
        BinaryPrimitives.WriteUInt32LittleEndian(b.AsSpan(8), (uint)options.Case);
        BinaryPrimitives.WriteUInt32LittleEndian(
            b.AsSpan(12), options.IncludeHiddenSystem ? 1u : 0u);
        textBytes.CopyTo(b, 16);
        return b;
    }

    public static (SearchOptions Options, string Text) DecodeQueryReq(ReadOnlySpan<byte> payload)
    {
        if (payload.Length < 16)
        {
            throw new InvalidDataException($"QueryReq payload is {payload.Length} bytes, need ≥16");
        }
        var options = new SearchOptions(
            (FmfSort)BinaryPrimitives.ReadUInt32LittleEndian(payload),
            BinaryPrimitives.ReadUInt32LittleEndian(payload[4..]) != 0,
            (FmfCase)BinaryPrimitives.ReadUInt32LittleEndian(payload[8..]),
            BinaryPrimitives.ReadUInt32LittleEndian(payload[12..]) != 0);
        return (options, Encoding.UTF8.GetString(payload[16..]));
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

    /// <summary>`{row_count:u32, blob_len:u32}` + 48B rows + WTF-8 blob.</summary>
    public static List<RowData> DecodePageResp(ReadOnlySpan<byte> payload)
    {
        if (payload.Length < 8)
        {
            throw new InvalidDataException($"PageResp payload is {payload.Length} bytes, need ≥8");
        }
        var rowCount = (int)BinaryPrimitives.ReadUInt32LittleEndian(payload);
        var blobLen = (int)BinaryPrimitives.ReadUInt32LittleEndian(payload[4..]);
        if (payload.Length != 8 + rowCount * RowSize + blobLen)
        {
            throw new InvalidDataException(
                $"PageResp payload is {payload.Length} bytes, "
                + $"expected {8 + rowCount * RowSize + blobLen} for {rowCount} rows");
        }
        return PageCodec.Decode(
            payload.Slice(8, rowCount * RowSize),
            payload.Slice(8 + rowCount * RowSize, blobLen));
    }

    public static byte[] EncodePageResp(IReadOnlyList<RowData> rows)
    {
        var blob = new List<byte>();
        var rowBytes = new byte[rows.Count * RowSize];
        for (var i = 0; i < rows.Count; i++)
        {
            var row = rows[i];
            var name = Wtf8.Encode(row.Name);
            var parent = Wtf8.Encode(row.ParentPath);
            var nameOff = (uint)blob.Count;
            blob.AddRange(name);
            var parentOff = (uint)blob.Count;
            blob.AddRange(parent);

            var r = rowBytes.AsSpan(i * RowSize, RowSize);
            BinaryPrimitives.WriteUInt64LittleEndian(r, row.EntryRef);
            BinaryPrimitives.WriteUInt64LittleEndian(r[8..], row.Frn);
            BinaryPrimitives.WriteUInt64LittleEndian(r[16..], row.Size);
            BinaryPrimitives.WriteInt64LittleEndian(r[24..], row.Mtime);
            BinaryPrimitives.WriteUInt32LittleEndian(r[32..], nameOff);
            BinaryPrimitives.WriteUInt32LittleEndian(r[36..], parentOff);
            BinaryPrimitives.WriteUInt32LittleEndian(r[40..], row.Flags);
            BinaryPrimitives.WriteUInt16LittleEndian(r[44..], (ushort)name.Length);
            BinaryPrimitives.WriteUInt16LittleEndian(r[46..], (ushort)parent.Length);
        }
        var b = new byte[8 + rowBytes.Length + blob.Count];
        BinaryPrimitives.WriteUInt32LittleEndian(b, (uint)rows.Count);
        BinaryPrimitives.WriteUInt32LittleEndian(b.AsSpan(4), (uint)blob.Count);
        rowBytes.CopyTo(b, 8);
        blob.CopyTo(b, 8 + rowBytes.Length);
        return b;
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

    // ── JSON payloads (op 4/5/6/10/12, snake_case) ──────────────────────

    private static readonly JsonSerializerOptions JsonOpts = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.SnakeCaseLower,
    };

    private sealed class VolumeStatusJson
    {
        public string Volume { get; set; } = string.Empty;
        public uint State { get; set; }
        public ulong Entries { get; set; }
    }

    private sealed class IndexStartJson
    {
        public List<string> Volumes { get; set; } = [];
    }

    /// <summary>`[{"volume":"C:","state":1,"entries":42}]` — ListVolumes and
    /// IndexStatus share this shape; state values equal VolumeState.</summary>
    public static List<VolumeStatus> DecodeVolumeStatuses(ReadOnlySpan<byte> payload)
    {
        var wire = JsonSerializer.Deserialize<List<VolumeStatusJson>>(payload, JsonOpts) ?? [];
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
        return JsonSerializer.SerializeToUtf8Bytes(wire, JsonOpts);
    }

    public static byte[] EncodeIndexStartReq(IReadOnlyList<string> volumes) =>
        JsonSerializer.SerializeToUtf8Bytes(new IndexStartJson { Volumes = [.. volumes] }, JsonOpts);

    public static IReadOnlyList<string> DecodeIndexStartReq(ReadOnlySpan<byte> payload) =>
        (JsonSerializer.Deserialize<IndexStartJson>(payload, JsonOpts) ?? new()).Volumes;

    private static void CheckLen(string what, ReadOnlySpan<byte> payload, int expected)
    {
        if (payload.Length != expected)
        {
            throw new InvalidDataException(
                $"{what} payload is {payload.Length} bytes, expected {expected}");
        }
    }
}
