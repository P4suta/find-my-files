using System.Text;
using FindMyFiles.Engine;
using Xunit;

namespace FindMyFiles.Tests;

/// <summary>
/// Pins the wire bytes to the exact values fmf-proto's Rust tests pin —
/// both suites must agree byte-for-byte or one of them fails.
/// </summary>
public sealed class PipeProtocolTests
{
    [Fact]
    public void Header_GoldenBytes_MatchTheRustPin()
    {
        var h = new PipeProtocol.FrameHeader(
            Len: 0x00010203,
            Opcode: 0x0506,
            Flags: 0x0003,
            RequestId: 0x0708090A,
            StatusCode: -2);
        var bytes = new byte[PipeProtocol.HeaderLen];

        PipeProtocol.WriteHeader(bytes, h);

        Assert.Equal(
            new byte[]
            {
                0x03, 0x02, 0x01, 0x00, // len
                0x06, 0x05, // opcode
                0x03, 0x00, // flags
                0x0A, 0x09, 0x08, 0x07, // request_id
                0xFE, 0xFF, 0xFF, 0xFF, // status (-2)
            },
            bytes);
        Assert.Equal(h, PipeProtocol.ReadHeader(bytes));
        Assert.True(h.IsResponse);
        Assert.True(h.IsEvent);
    }

    [Fact]
    public void QueryReq_GoldenBytes_MatchTheRustPin()
    {
        var opts = new SearchOptions(
            FmfSort.Size,
            Descending: true,
            FmfCase.Sensitive,
            IncludeHiddenSystem: false,
            RegexMode: true,
            Scope: RegexScope.Path);
        var bytes = PipeProtocol.EncodeQueryReq(opts, "win");

        Assert.Equal(
            new byte[]
            {
                1, 0, 0, 0, // sort = Size
                1, 0, 0, 0, // desc
                2, 0, 0, 0, // case = Sensitive
                0, 0, 0, 0, // include_hidden_system
                3, 0, 0, 0, // regex_mode = whole(bit0) | path(bit1)
                (byte)'w', (byte)'i', (byte)'n',
            },
            bytes);

        var (options, text) = PipeProtocol.DecodeQueryReq(bytes);
        Assert.Equal(opts, options);
        Assert.Equal("win", text);
    }

    [Fact]
    public void HelloResp_GoldenBytes_MatchTheRustPin()
    {
        var bytes = PipeProtocol.EncodeHelloResp(1, 1, 0x04030201);

        Assert.Equal(new byte[] { 1, 0, 0, 0, 1, 0, 0, 0, 1, 2, 3, 4 }, bytes);
        Assert.Equal((1u, 1u, 0x04030201u), PipeProtocol.DecodeHelloResp(bytes));
    }

    [Fact]
    public void Header_OversizedPayload_IsRejected()
    {
        var bytes = new byte[PipeProtocol.HeaderLen];
        PipeProtocol.WriteHeader(
            bytes, new PipeProtocol.FrameHeader(PipeProtocol.MaxPayloadLen + 1, 1, 0, 1, 0));

        Assert.Throws<InvalidDataException>(() => PipeProtocol.ReadHeader(bytes));
    }

    [Fact]
    public void VolumeStatusJson_IsSnakeCase_AndRoundTrips()
    {
        var bytes = PipeProtocol.EncodeVolumeStatuses([new("C:", VolumeState.Ready, 42)]);

        Assert.Equal("""[{"volume":"C:","state":1,"entries":42}]""", Encoding.UTF8.GetString(bytes));
        Assert.Equal(
            new VolumeStatus("C:", VolumeState.Ready, 42),
            Assert.Single(PipeProtocol.DecodeVolumeStatuses(bytes)));
    }

    [Fact]
    public void IndexStartReq_IsSnakeCaseJson()
    {
        var bytes = PipeProtocol.EncodeIndexStartReq(["C:", "D:"]);

        Assert.Equal("""{"volumes":["C:","D:"]}""", Encoding.UTF8.GetString(bytes));
        Assert.Equal(["C:", "D:"], PipeProtocol.DecodeIndexStartReq(bytes));
    }

    [Fact]
    public void Event_RoundTrips_WithZeroPaddedLabel()
    {
        var bytes = PipeProtocol.EncodeEvent(3, 7, "C:");

        Assert.Equal(32, bytes.Length);
        Assert.Equal((3u, 7ul, "C:"), PipeProtocol.DecodeEvent(bytes));
    }

    [Fact]
    public void PageResp_RoundTrips_IncludingMultiByteNames()
    {
        List<RowData> rows =
        [
            new(1, 100, 10, 1111, 0, "省察.txt", "C:\\メモ\\"),
            new(2, 200, 20, 2222, 1, "b", "C:\\"),
        ];

        var decoded = PipeProtocol.DecodePageResp(PipeProtocol.EncodePageResp(rows));

        Assert.Equal(rows, decoded);
    }

    [Fact]
    public void PageResp_LyingLengths_AreRejected()
    {
        var bytes = PipeProtocol.EncodePageResp([new(1, 1, 1, 1, 0, "a", "C:\\")]);
        bytes[0] = 2; // row_count says 2, but only one row is present

        Assert.Throws<InvalidDataException>(() => PipeProtocol.DecodePageResp(bytes));
    }
}
