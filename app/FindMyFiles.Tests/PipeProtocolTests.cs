using System.Buffers.Binary;
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
        const ulong presentationBasis = 0x0102_0304_0506_0708;
        var bytes = PipeProtocol.EncodeQueryReq(opts, "win", presentationBasis);

        Assert.Equal(
            new byte[]
            {
                1, 0, 0, 0, // sort = Size
                1, 0, 0, 0, // desc
                2, 0, 0, 0, // case = Sensitive
                0, 0, 0, 0, // include_hidden_system
                3, 0, 0, 0, // regex_mode = whole(bit0) | path(bit1)
                0, 0, 0, 0, // reserved
                8, 7, 6, 5, 4, 3, 2, 1, // presentation basis
                (byte)'w', (byte)'i', (byte)'n',
            },
            bytes);

        var (options, text, decodedPresentationBasis) = PipeProtocol.DecodeQueryReq(bytes);
        Assert.Equal(opts, options);
        Assert.Equal("win", text);
        Assert.Equal(presentationBasis, decodedPresentationBasis);
    }

    [Fact]
    public void QueryReq_ExactlyTheOptionsSize_DecodesAnEmptyQuery()
    {
        var (options, text, presentationBasis) = PipeProtocol.DecodeQueryReq(
            new byte[EngineContract.QueryOptionsSize]);

        Assert.Equal(SearchOptions.Default, options);
        Assert.Equal(string.Empty, text);
        Assert.Equal(0UL, presentationBasis);
    }

    [Fact]
    public void QueryReq_NonZeroReservedField_IsRejected()
    {
        var payload = new byte[EngineContract.QueryOptionsSize];
        BinaryPrimitives.WriteUInt32LittleEndian(payload.AsSpan(20), 1);

        var ex = Assert.Throws<InvalidDataException>(() => PipeProtocol.DecodeQueryReq(payload));

        Assert.Equal("QueryReq reserved field is nonzero", ex.Message);
    }

    [Fact]
    public void EncodeQueryReq_RejectsTextOverTheUtf8ByteLimit()
    {
        var text = new string('界', (EngineContract.MaxQueryBytes / 3) + 1);

        Assert.Throws<ArgumentException>(
            () => PipeProtocol.EncodeQueryReq(SearchOptions.Default, text));
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

    [Theory]
    [InlineData(0UL)]
    [InlineData(4UL)]
    [InlineData(ulong.MaxValue)]
    public void EngineErrorSeverityWire_RejectsValuesOutsideTheContract(ulong value)
    {
        var ex = Assert.Throws<InvalidDataException>(() => EngineErrorSeverityWire.Decode(value));

        Assert.Equal($"engine-error severity {value} is outside the contract", ex.Message);
    }

    [Fact]
    public void EngineErrorSeverityWire_AcceptsEveryContractValue()
    {
        EngineErrorSeverity[] severities =
        [
            EngineErrorSeverity.Warn,
            EngineErrorSeverity.Error,
            EngineErrorSeverity.Panic,
        ];
        foreach (var severity in severities)
        {
            Assert.Equal(severity, EngineErrorSeverityWire.Decode((ulong)severity));
        }
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
        var bytes = PipeProtocol.EncodeIndexStartReq(["c:", "D:"]);

        Assert.Equal("""{"volumes":["C:","D:"]}""", Encoding.UTF8.GetString(bytes));
        Assert.Equal(["C:", "D:"], PipeProtocol.DecodeIndexStartReq(bytes));
    }

    [Theory]
    [InlineData("{}")]
    [InlineData("null")]
    public void IndexStartReq_MissingVolumes_DecodesToEmptyList(string json) =>
        Assert.Empty(PipeProtocol.DecodeIndexStartReq(Encoding.UTF8.GetBytes(json)));

    [Fact]
    public void IndexStartReq_RejectsMalformedAndUnboundedLists()
    {
        Assert.Throws<ArgumentException>(
            () => PipeProtocol.EncodeIndexStartReq(["..\\escape"]));
        Assert.Throws<ArgumentException>(
            () => PipeProtocol.EncodeIndexStartReq(["C:", "c:"]));
        Assert.Throws<ArgumentOutOfRangeException>(
            () => PipeProtocol.EncodeIndexStartReq(
                Enumerable.Repeat("C:", EngineContract.MaxVolumes + 1).ToArray()));
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

        var ex = Assert.Throws<InvalidDataException>(() => PipeProtocol.DecodePageResp(bytes));

        Assert.Equal(
            $"PageResp payload is {bytes.Length} bytes, expected {8 + (2 * PipeProtocol.RowSize) + 4} for 2 rows",
            ex.Message);
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

    [Fact]
    public void PageResp_RejectsMoreThanTheContractRowLimit()
    {
        var rows = Enumerable
            .Range(0, EngineContract.MaxPageRows + 1)
            .Select(i => new RowData((ulong)i, (ulong)i, 0, 0, 0, "n", "C:\\"))
            .ToArray();

        var ex = Assert.Throws<ArgumentOutOfRangeException>(() => PipeProtocol.EncodePageResp(rows));

        Assert.Contains(
            $"A page may contain at most {EngineContract.MaxPageRows} rows.",
            ex.Message,
            StringComparison.Ordinal);
    }

    [Fact]
    public void EncodePageResp_ExactlyTheContractRowLimit_IsAccepted()
    {
        var rows = Enumerable
            .Range(0, EngineContract.MaxPageRows)
            .Select(i => new RowData((ulong)i, (ulong)i, 0, 0, 0, string.Empty, string.Empty))
            .ToArray();

        Assert.Equal(
            8 + (EngineContract.MaxPageRows * PipeProtocol.RowSize),
            PipeProtocol.EncodePageResp(rows).Length);
    }

    [Fact]
    public void DecodePageResp_ExactlyTheContractRowLimit_IsAccepted()
    {
        var payload = new byte[8 + (EngineContract.MaxPageRows * PipeProtocol.RowSize)];
        BinaryPrimitives.WriteUInt32LittleEndian(payload, (uint)EngineContract.MaxPageRows);

        Assert.Equal(EngineContract.MaxPageRows, PipeProtocol.DecodePageResp(payload).Count);
    }

    [Fact]
    public void DecodePageResp_RejectsAHostileRowCountBeforeSizingRows()
    {
        var payload = new byte[8];
        BinaryPrimitives.WriteUInt32LittleEndian(
            payload, (uint)EngineContract.MaxPageRows + 1);

        var ex = Assert.Throws<InvalidDataException>(() => PipeProtocol.DecodePageResp(payload));

        Assert.Equal(
            $"PageResp row_count {EngineContract.MaxPageRows + 1} exceeds {EngineContract.MaxPageRows}",
            ex.Message);
    }

    [Fact]
    public void PagePayloadLengthValidators_AcceptTheCapWithoutAllocation()
    {
        var max = checked((int)PipeProtocol.MaxPayloadLen);

        Assert.Equal(max, PipeProtocol.ValidateDecodedPagePayloadLength(max));
        Assert.Equal(max, PipeProtocol.ValidateEncodedPagePayloadLength(max));
    }

    [Fact]
    public void PagePayloadLengthValidators_RejectOneByteOverTheCap()
    {
        var max = checked((int)PipeProtocol.MaxPayloadLen);
        var decode = Assert.Throws<InvalidDataException>(
            () => PipeProtocol.ValidateDecodedPagePayloadLength(max + 1));
        Assert.Equal(
            $"PageResp payload is {max + 1} bytes, maximum is {PipeProtocol.MaxPayloadLen}",
            decode.Message);

        var encode = Assert.Throws<ArgumentOutOfRangeException>(
            () => PipeProtocol.ValidateEncodedPagePayloadLength(max + 1L));
        Assert.Contains(
            $"Encoded page is {max + 1} bytes; maximum is {PipeProtocol.MaxPayloadLen}.",
            encode.Message,
            StringComparison.Ordinal);
    }

    [Fact]
    public void EncodePageResp_RejectsNullRows()
    {
        Assert.Throws<ArgumentNullException>(() => PipeProtocol.EncodePageResp(null!));
    }

    [Theory]
    [InlineData(0)]
    [InlineData(7)] // one short of the 8-byte {row_count, blob_len} header
    public void PageResp_TruncatedHeader_IsRejected(int len)
    {
        var ex = Assert.Throws<InvalidDataException>(
            () => PipeProtocol.DecodePageResp(new byte[len]));

        Assert.Equal($"PageResp payload is {len} bytes, need ≥8", ex.Message);
    }

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
    public void QueryResp_ExactlyTheFixedFields_DecodesAnEmptyTrace()
    {
        Assert.Equal((0UL, 0UL, string.Empty), PipeProtocol.DecodeQueryResp(new byte[16]));
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

    [Fact]
    public void Event_EmptyLabel_StopsAtTheFirstTerminator()
    {
        Assert.Equal(string.Empty, PipeProtocol.DecodeEvent(new byte[32]).Volume);
    }

    [Theory]
    [InlineData(11)]
    [InlineData(13)]
    public void DecodeHelloResp_WrongLength_IsRejected(int len)
    {
        var ex = Assert.Throws<InvalidDataException>(
            () => PipeProtocol.DecodeHelloResp(new byte[len]));

        Assert.Equal($"HelloResp payload is {len} bytes, expected 12", ex.Message);
    }

    [Fact]
    public void DecodeQueryReq_TooShortForOptions_IsRejected()
    {
        var len = EngineContract.QueryOptionsSize - 1;
        var ex = Assert.Throws<InvalidDataException>(
            () => PipeProtocol.DecodeQueryReq(new byte[len]));

        Assert.Equal(
            $"QueryReq payload is {len} bytes, need ≥{EngineContract.QueryOptionsSize}",
            ex.Message);
    }

    [Fact]
    public void DecodeQueryResp_TooShortForIds_IsRejected()
    {
        var ex = Assert.Throws<InvalidDataException>(
            () => PipeProtocol.DecodeQueryResp(new byte[15]));

        Assert.Equal("QueryResp payload is 15 bytes, need ≥16", ex.Message);
    }

    [Theory]
    [InlineData(19)]
    [InlineData(21)]
    public void DecodeResultPageReq_WrongLength_IsRejected(int len)
    {
        var ex = Assert.Throws<InvalidDataException>(
            () => PipeProtocol.DecodeResultPageReq(new byte[len]));

        Assert.Equal($"ResultPageReq payload is {len} bytes, expected 20", ex.Message);
    }

    [Theory]
    [InlineData(7)]
    [InlineData(9)]
    public void DecodeResultFreeReq_WrongLength_IsRejected(int len)
    {
        var ex = Assert.Throws<InvalidDataException>(
            () => PipeProtocol.DecodeResultFreeReq(new byte[len]));

        Assert.Equal($"ResultFreeReq payload is {len} bytes, expected 8", ex.Message);
    }

    [Theory]
    [InlineData(31)]
    [InlineData(33)]
    public void DecodeEvent_WrongLength_IsRejected(int len)
    {
        var ex = Assert.Throws<InvalidDataException>(
            () => PipeProtocol.DecodeEvent(new byte[len]));

        Assert.Equal($"Event payload is {len} bytes, expected 32", ex.Message);
    }

    [Fact]
    public void DecodeVolumeStatuses_EmptyJsonArray_DecodesToNoStatuses() =>
        Assert.Empty(PipeProtocol.DecodeVolumeStatuses("[]"u8));

    [Fact]
    public void DecodeVolumeStatuses_JsonNull_DecodesToNoStatuses() =>
        Assert.Empty(PipeProtocol.DecodeVolumeStatuses("null"u8));

    [Fact]
    public void DecodeVolumeStatuses_MissingVolume_UsesTheEmptyWireDefault()
    {
        var status = Assert.Single(
            PipeProtocol.DecodeVolumeStatuses("[{\"state\":1,\"entries\":2}]"u8));

        Assert.Equal(string.Empty, status.Label);
        Assert.Equal(VolumeState.Ready, status.State);
        Assert.Equal(2UL, status.Entries);
    }
}
