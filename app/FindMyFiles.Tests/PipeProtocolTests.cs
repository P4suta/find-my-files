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

        var ex = Assert.Throws<InvalidDataException>(() => PipeProtocol.ReadHeader(bytes));

        // The drop reason must name the cap — there is no resync point, so this
        // message is the only forensic trail for a hostile/oversized frame.
        Assert.Contains(PipeProtocol.MaxPayloadLen.ToString(), ex.Message, StringComparison.Ordinal);
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

    [Fact]
    public void PageResp_LyingBlobLength_IsRejected()
    {
        var bytes = PipeProtocol.EncodePageResp([new(1, 1, 1, 1, 0, "ab", "C:\\")]);
        bytes[4]++; // blob_len overstated by one — total no longer matches

        Assert.Throws<InvalidDataException>(() => PipeProtocol.DecodePageResp(bytes));
    }

    [Fact]
    public void PageResp_EmptyPage_RoundTripsToNoRows()
    {
        var bytes = PipeProtocol.EncodePageResp([]);

        Assert.Equal(8, bytes.Length); // just the {row_count, blob_len} header
        Assert.Empty(PipeProtocol.DecodePageResp(bytes));
    }

    [Theory]
    [InlineData(0)]
    [InlineData(7)] // one short of the 8-byte {row_count, blob_len} header
    public void PageResp_TruncatedHeader_IsRejected(int len) =>
        Assert.Throws<InvalidDataException>(() => PipeProtocol.DecodePageResp(new byte[len]));

    [Fact]
    public void Header_AtTheExactCap_IsAccepted()
    {
        // The boundary is `> MaxPayloadLen`, so the cap value itself is legal.
        var bytes = new byte[PipeProtocol.HeaderLen];
        PipeProtocol.WriteHeader(
            bytes, new PipeProtocol.FrameHeader(PipeProtocol.MaxPayloadLen, 1, 0, 1, 0));

        Assert.Equal(PipeProtocol.MaxPayloadLen, PipeProtocol.ReadHeader(bytes).Len);
    }

    [Fact]
    public void Header_PlainRequest_IsNeitherResponseNorEvent()
    {
        var h = new PipeProtocol.FrameHeader(0, PipeProtocol.Op.Query, 0, 9, 0);

        Assert.False(h.IsResponse);
        Assert.False(h.IsEvent);
    }

    [Fact]
    public void EncodeFrame_PrependsTheHeaderWithThePayloadLength()
    {
        byte[] payload = [0xAA, 0xBB, 0xCC];

        var frame = PipeProtocol.EncodeFrame(
            PipeProtocol.Op.Query, PipeProtocol.FlagResponse, 0x11223344, 0, payload);

        var header = PipeProtocol.ReadHeader(frame);
        Assert.Equal((uint)payload.Length, header.Len);
        Assert.Equal(PipeProtocol.Op.Query, header.Opcode);
        Assert.Equal(0x11223344u, header.RequestId);
        Assert.True(header.IsResponse);
        Assert.Equal(payload, frame.AsSpan(PipeProtocol.HeaderLen).ToArray());
    }

    [Fact]
    public void QueryResp_RoundTrips_WithTraceJson()
    {
        var bytes = PipeProtocol.EncodeQueryResp(0xDEAD_BEEF_0000_0001, 42, """{"q":"x"}""");

        Assert.Equal(
            (0xDEAD_BEEF_0000_0001UL, 42UL, """{"q":"x"}"""),
            PipeProtocol.DecodeQueryResp(bytes));
    }

    [Fact]
    public void ResultPageReq_RoundTrips()
    {
        var bytes = PipeProtocol.EncodeResultPageReq(0x0102_0304_0506_0708, 0x1000, 250);

        Assert.Equal(
            (0x0102_0304_0506_0708UL, 0x1000UL, 250U),
            PipeProtocol.DecodeResultPageReq(bytes));
    }

    [Fact]
    public void ResultFreeReq_RoundTrips()
    {
        var bytes = PipeProtocol.EncodeResultFreeReq(0xABCD_1234_5678_9ABC);

        Assert.Equal(0xABCD_1234_5678_9ABCUL, PipeProtocol.DecodeResultFreeReq(bytes));
    }

    [Fact]
    public void Event_FullSixteenByteLabel_DecodesWithoutATerminator()
    {
        // No NUL inside the 16-byte volume field: the decoder must read all 16
        // bytes (the `len < 0 → 16` fallback), not stop early.
        var payload = new byte[32];
        var label = "0123456789ABCDEF"u8; // exactly 16 bytes, no terminator
        label.CopyTo(payload.AsSpan(16));

        var (_, _, volume) = PipeProtocol.DecodeEvent(payload);

        Assert.Equal("0123456789ABCDEF", volume);
    }

    [Theory]
    [InlineData(11)]
    [InlineData(13)]
    public void DecodeHelloResp_WrongLength_IsRejected(int len) =>
        Assert.Throws<InvalidDataException>(() => PipeProtocol.DecodeHelloResp(new byte[len]));

    [Fact]
    public void DecodeQueryReq_TooShortForOptions_IsRejected() =>
        Assert.Throws<InvalidDataException>(
            () => PipeProtocol.DecodeQueryReq(new byte[EngineContract.QueryOptionsSize - 1]));

    [Fact]
    public void DecodeQueryResp_TooShortForIds_IsRejected() =>
        Assert.Throws<InvalidDataException>(() => PipeProtocol.DecodeQueryResp(new byte[15]));

    [Theory]
    [InlineData(19)]
    [InlineData(21)]
    public void DecodeResultPageReq_WrongLength_IsRejected(int len) =>
        Assert.Throws<InvalidDataException>(() => PipeProtocol.DecodeResultPageReq(new byte[len]));

    [Theory]
    [InlineData(7)]
    [InlineData(9)]
    public void DecodeResultFreeReq_WrongLength_IsRejected(int len) =>
        Assert.Throws<InvalidDataException>(() => PipeProtocol.DecodeResultFreeReq(new byte[len]));

    [Theory]
    [InlineData(31)]
    [InlineData(33)]
    public void DecodeEvent_WrongLength_IsRejected(int len) =>
        Assert.Throws<InvalidDataException>(() => PipeProtocol.DecodeEvent(new byte[len]));

    [Fact]
    public void DecodeVolumeStatuses_EmptyJsonArray_DecodesToNoStatuses() =>
        Assert.Empty(PipeProtocol.DecodeVolumeStatuses("[]"u8));
}
