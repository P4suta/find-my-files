using System.Buffers.Binary;

namespace FindMyFiles.Engine;

/// <summary>
/// Decodes the shared page layout — densely packed fixed-size rows followed by
/// one WTF-8 string blob — used verbatim by both the FFI <c>FmfPage</c> and the
/// pipe ResultPage payload. Every row's name/parent-path offset is relative to
/// the start of that blob, not to the row or the frame. Row size and field
/// offsets come from <see cref="EngineContract.RowOffsets"/>.
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
            var nameLen = BinaryPrimitives.ReadUInt32LittleEndian(
                r[EngineContract.RowOffsets.NameLen..]);
            var parentPathLen = BinaryPrimitives.ReadUInt32LittleEndian(
                r[EngineContract.RowOffsets.ParentPathLen..]);
            var flags = BinaryPrimitives.ReadUInt32LittleEndian(
                r[EngineContract.RowOffsets.Flags..]);
            var unknownFlags = flags & ~EngineContract.RowFlags.KnownMask;
            if (unknownFlags != 0)
            {
                throw new InvalidDataException(
                    $"row {i} has unknown flags (0x{unknownFlags:X8})");
            }

            var reserved = BinaryPrimitives.ReadUInt32LittleEndian(
                r[EngineContract.RowOffsets.Reserved..]);
            if (reserved != 0)
            {
                throw new InvalidDataException(
                    $"row {i} has non-zero reserved field ({reserved})");
            }

            rows.Add(new RowData(
                EntryRef: BinaryPrimitives.ReadUInt64LittleEndian(
                    r[EngineContract.RowOffsets.EntryRef..]),
                Frn: BinaryPrimitives.ReadUInt64LittleEndian(
                    r[EngineContract.RowOffsets.Frn..]),
                Size: BinaryPrimitives.ReadUInt64LittleEndian(
                    r[EngineContract.RowOffsets.Size..]),
                Mtime: BinaryPrimitives.ReadInt64LittleEndian(
                    r[EngineContract.RowOffsets.Mtime..]),
                Flags: flags,
                Name: Wtf8.Decode(BlobWindow(blob, nameOff, nameLen, i, "name")),
                ParentPath: Wtf8.Decode(
                    BlobWindow(blob, parentPathOff, parentPathLen, i, "parent path"))));
        }

        return rows;
    }

    private static ReadOnlySpan<byte> BlobWindow(
        ReadOnlySpan<byte> blob,
        uint offset,
        uint length,
        int row,
        string field)
    {
        var end = (ulong)offset + length;
        if (end > (ulong)blob.Length)
        {
            throw new InvalidDataException(
                $"row {row} {field} window [{offset}, {end}) exceeds blob length {blob.Length}");
        }

        // The ulong end check above proves both uint values fit in the int-sized
        // span before either cast is evaluated.
        return blob.Slice((int)offset, (int)length);
    }
}
