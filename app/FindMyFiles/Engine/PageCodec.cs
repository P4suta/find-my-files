using System.Buffers.Binary;

namespace FindMyFiles.Engine;

/// <summary>
/// Decodes the shared page layout — 48-byte rows + WTF-8 string blob — used
/// verbatim by both the FFI <c>FmfPage</c> and the pipe ResultPage payload
/// (docs/ARCHITECTURE.md: FmfRow layout, offsets are blob-relative).
/// </summary>
internal static class PageCodec
{
    // Offsets radiate from Generated/EngineContract.g.cs — the Rust
    // offset_of! values, no hand-derived numbers (ADR-0018).
    public const int RowSize = EngineContract.RowSize;

    public static List<RowData> Decode(ReadOnlySpan<byte> rowBytes, ReadOnlySpan<byte> blob)
    {
        if (rowBytes.Length % RowSize != 0)
        {
            throw new ArgumentException(
                $"row bytes ({rowBytes.Length}) are not a multiple of {RowSize}", nameof(rowBytes));
        }
        var count = rowBytes.Length / RowSize;
        var rows = new List<RowData>(count);
        for (var i = 0; i < count; i++)
        {
            var r = rowBytes.Slice(i * RowSize, RowSize);
            var nameOff = BinaryPrimitives.ReadUInt32LittleEndian(
                r[EngineContract.RowOffsets.NameOff..]);
            var parentPathOff = BinaryPrimitives.ReadUInt32LittleEndian(
                r[EngineContract.RowOffsets.ParentPathOff..]);
            var nameLen = BinaryPrimitives.ReadUInt16LittleEndian(
                r[EngineContract.RowOffsets.NameLen..]);
            var parentPathLen = BinaryPrimitives.ReadUInt16LittleEndian(
                r[EngineContract.RowOffsets.ParentPathLen..]);
            rows.Add(new RowData(
                EntryRef: BinaryPrimitives.ReadUInt64LittleEndian(
                    r[EngineContract.RowOffsets.EntryRef..]),
                Frn: BinaryPrimitives.ReadUInt64LittleEndian(
                    r[EngineContract.RowOffsets.Frn..]),
                Size: BinaryPrimitives.ReadUInt64LittleEndian(
                    r[EngineContract.RowOffsets.Size..]),
                Mtime: BinaryPrimitives.ReadInt64LittleEndian(
                    r[EngineContract.RowOffsets.Mtime..]),
                Flags: BinaryPrimitives.ReadUInt32LittleEndian(
                    r[EngineContract.RowOffsets.Flags..]),
                Name: Wtf8.Decode(blob.Slice((int)nameOff, nameLen)),
                ParentPath: Wtf8.Decode(blob.Slice((int)parentPathOff, parentPathLen))));
        }
        return rows;
    }
}
