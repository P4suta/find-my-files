using System.Buffers.Binary;

namespace FindMyFiles.Engine;

/// <summary>
/// Decodes the shared page layout — 48-byte rows + WTF-8 string blob — used
/// verbatim by both the FFI <c>FmfPage</c> and the pipe ResultPage payload
/// (docs/ARCHITECTURE.md: FmfRow layout, offsets are blob-relative).
/// </summary>
internal static class PageCodec
{
    public const int RowSize = 48;

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
            var nameOff = BinaryPrimitives.ReadUInt32LittleEndian(r[32..]);
            var parentPathOff = BinaryPrimitives.ReadUInt32LittleEndian(r[36..]);
            var nameLen = BinaryPrimitives.ReadUInt16LittleEndian(r[44..]);
            var parentPathLen = BinaryPrimitives.ReadUInt16LittleEndian(r[46..]);
            rows.Add(new RowData(
                EntryRef: BinaryPrimitives.ReadUInt64LittleEndian(r),
                Frn: BinaryPrimitives.ReadUInt64LittleEndian(r[8..]),
                Size: BinaryPrimitives.ReadUInt64LittleEndian(r[16..]),
                Mtime: BinaryPrimitives.ReadInt64LittleEndian(r[24..]),
                Flags: BinaryPrimitives.ReadUInt32LittleEndian(r[40..]),
                Name: Wtf8.Decode(blob.Slice((int)nameOff, nameLen)),
                ParentPath: Wtf8.Decode(blob.Slice((int)parentPathOff, parentPathLen))));
        }
        return rows;
    }
}
