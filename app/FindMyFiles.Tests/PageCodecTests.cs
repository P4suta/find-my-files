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
        Assert.Throws<ArgumentException>(() => PageCodec.Decode(new byte[47], []));
    }
}
