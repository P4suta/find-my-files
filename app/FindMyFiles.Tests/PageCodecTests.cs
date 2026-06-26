using System.Text;
using FindMyFiles.Engine;
using Xunit;

namespace FindMyFiles.Tests;

public sealed class PageCodecTests
{
    /// <summary>The same row fmf-proto's `page_roundtrip_pins_the_48_byte_row`
    /// pins on the Rust side — layout drift fails one suite or the other.</summary>
    private static readonly byte[] GoldenRow =
    [
        0x01, 0, 0, 0, 0, 0, 0, 0, // entry_ref = 1
        0x64, 0, 0, 0, 0, 0, 0x01, 0, // frn = (1<<48)|100
        0xD2, 0x04, 0, 0, 0, 0, 0, 0, // size = 1234
        0xFB, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, // mtime = -5
        0, 0, 0, 0, // name_off = 0
        0x09, 0, 0, 0, // parent_path_off = 9
        0x01, 0, 0, 0, // flags = 1 (directory)
        0x09, 0, // name_len = 9
        0x03, 0, // parent_path_len = 3
    ];

    [Fact]
    public void GoldenRow_DecodesAgainstTheRustPin()
    {
        var blob = Encoding.UTF8.GetBytes("alpha.txtC:\\");

        var row = Assert.Single(PageCodec.Decode(GoldenRow, blob));

        Assert.Equal(
            new RowData(
                EntryRef: 1,
                Frn: (1UL << 48) | 100,
                Size: 1234,
                Mtime: -5,
                Flags: 1,
                Name: "alpha.txt",
                ParentPath: "C:\\"),
            row);
        Assert.True(row.IsDirectory);
        Assert.Equal("C:\\alpha.txt", row.FullPath);
    }

    [Fact]
    public void EmptyPage_DecodesToNoRows()
    {
        Assert.Empty(PageCodec.Decode([], []));
    }

    [Fact]
    public void LoneSurrogate_SurvivesWtf8()
    {
        // WTF-8 ED A0 80 = unpaired U+D800, preserved verbatim (never U+FFFD).
        var blob = new byte[] { 0xED, 0xA0, 0x80 };
        var rowBytes = new byte[PageCodec.RowSize];
        rowBytes[44] = 3; // name_len = 3; every other field stays zero

        var row = Assert.Single(PageCodec.Decode(rowBytes, blob));

        Assert.Equal("\uD800", row.Name);
        Assert.Equal(string.Empty, row.ParentPath);
        Assert.Equal(blob, Wtf8.Encode(row.Name)); // and it round-trips back
    }

    [Fact]
    public void MisalignedRowBytes_AreRejected()
    {
        var ex = Assert.Throws<ArgumentException>(() => PageCodec.Decode(new byte[47], []));

        // The diagnostic names the offending length and the row stride, so a
        // truncated buffer is debuggable from the message alone.
        Assert.Contains("47", ex.Message, StringComparison.Ordinal);
        Assert.Contains($"multiple of {PageCodec.RowSize}", ex.Message, StringComparison.Ordinal);
    }

    [Fact]
    public void MultipleRows_EachReadsItsOwnSliceAndBlobWindow()
    {
        // Two distinct rows back-to-back: the per-row offset (i * RowSize) and
        // the per-row blob windows must stay independent, so a swapped index
        // or a dropped multiply surfaces as a wrong field on the second row.
        List<RowData> rows =
        [
            new(EntryRef: 1, Frn: 10, Size: 100, Mtime: 5, Flags: 0, Name: "first.txt", ParentPath: "C:\\a\\"),
            new(EntryRef: 2, Frn: 20, Size: 200, Mtime: 6, Flags: 1, Name: "二.dat", ParentPath: "D:\\"),
        ];
        var page = PipeProtocol.EncodePageResp(rows);

        // Decode the row+blob spans directly through PageCodec (skip the 8-byte
        // PageResp header EncodePageResp prepends).
        var rowBytes = rows.Count * PageCodec.RowSize;
        var decoded = PageCodec.Decode(
            page.AsSpan(8, rowBytes),
            page.AsSpan(8 + rowBytes));

        Assert.Equal(rows, decoded);
        Assert.Equal("C:\\a\\first.txt", decoded[0].FullPath);
        Assert.False(decoded[0].IsDirectory);
        Assert.Equal("二.dat", decoded[1].Name);
        Assert.True(decoded[1].IsDirectory);
    }
}
