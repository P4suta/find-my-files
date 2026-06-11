using System.Buffers.Binary;
using System.Text;
using System.Text.Json;
using FindMyFiles.Engine;
using Xunit;

namespace FindMyFiles.Tests.Contract;

/// <summary>
/// Pins the shared golden corpus (contract/golden/, captured by the Rust
/// suite via the FMF_BLESS=1 ritual) against the C# codec: every frame must
/// decode and re-encode byte-for-byte. Rust and C# reading the same files is
/// what makes "Rust/C# 両テストが同一ゴールデンバイトをピンする" true
/// (ADR-0018). The C# codec stays hand-written on purpose — two independent
/// implementations agreeing on one corpus is the verification structure.
/// </summary>
public sealed class GoldenCorpusTests
{
    private static string GoldenDir =>
        Environment.GetEnvironmentVariable("FMF_GOLDEN_DIR")
        ?? Path.Combine(AppContext.BaseDirectory, "golden");

    private static readonly JsonSerializerOptions SnakeCase = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.SnakeCaseLower,
    };

    private static byte[] Load(string file) =>
        File.ReadAllBytes(Path.Combine(GoldenDir, file));

    private static IReadOnlyList<string> ManifestFiles()
    {
        using var doc = JsonDocument.Parse(Load("manifest.json"));
        return [.. doc.RootElement.GetProperty("cases").EnumerateArray()
            .Select(c => c.GetProperty("file").GetString()!)];
    }

    [Fact]
    public void Manifest_CoversEveryBinFile_AndViceVersa()
    {
        var manifest = ManifestFiles().ToHashSet();
        var onDisk = Directory.GetFiles(GoldenDir, "*.bin")
            .Select(Path.GetFileName)
            .ToHashSet();

        Assert.True(manifest.SetEquals(onDisk!),
            $"manifest/disk drift — manifest only: [{string.Join(", ", manifest.Except(onDisk!))}], "
            + $"disk only: [{string.Join(", ", onDisk!.Except(manifest))}]");
    }

    [Fact]
    public void EveryFrame_DecodesAndReencodes_ToTheExactGoldenBytes()
    {
        foreach (var file in ManifestFiles())
        {
            var bytes = Load(file);
            var header = PipeProtocol.ReadHeader(bytes.AsSpan(0, PipeProtocol.HeaderLen));
            var payload = bytes.AsSpan(PipeProtocol.HeaderLen).ToArray();
            Assert.True(header.Len == payload.Length, $"{file}: header len vs payload");

            var reencoded = ReencodePayload(file, header, payload);
            var frame = PipeProtocol.EncodeFrame(
                header.Opcode, header.Flags, header.RequestId, header.StatusCode, reencoded);

            Assert.True(bytes.AsSpan().SequenceEqual(frame),
                $"{file}: C# re-encode drifted from the golden bytes");
        }
    }

    /// <summary>Decode + re-encode through the C# codec; returns the payload
    /// verbatim for shapes C# only consumes (validated, no encoder).</summary>
    private static byte[] ReencodePayload(
        string file, PipeProtocol.FrameHeader header, byte[] payload)
    {
        if (header.IsEvent)
        {
            var (kind, entries, volume) = PipeProtocol.DecodeEvent(payload);
            Assert.True(kind == header.Opcode, $"{file}: event kind vs opcode");
            return PipeProtocol.EncodeEvent(kind, entries, volume);
        }
        if (header.StatusCode != 0)
        {
            // Error responses carry a UTF-8 detail string.
            return Encoding.UTF8.GetBytes(Encoding.UTF8.GetString(payload));
        }
        switch (header.Opcode, header.IsResponse)
        {
            case (PipeProtocol.Op.Hello, false):
                return PipeProtocol.EncodeHelloReq(
                    BinaryPrimitives.ReadUInt32LittleEndian(payload));
            case (PipeProtocol.Op.Hello, true):
            {
                var (proto, abi, pid) = PipeProtocol.DecodeHelloResp(payload);
                return PipeProtocol.EncodeHelloResp(proto, abi, pid);
            }
            case (PipeProtocol.Op.Subscribe, _) or (PipeProtocol.Op.Unsubscribe, _):
                Assert.True(payload.Length == 0, $"{file}: expected empty payload");
                return payload;
            case (PipeProtocol.Op.ListVolumes, true) or (PipeProtocol.Op.IndexStatus, true):
                return PipeProtocol.EncodeVolumeStatuses(
                    PipeProtocol.DecodeVolumeStatuses(payload));
            case (PipeProtocol.Op.IndexStart, false):
                return PipeProtocol.EncodeIndexStartReq(
                    PipeProtocol.DecodeIndexStartReq(payload));
            case (PipeProtocol.Op.Query, false):
            {
                var (options, text) = PipeProtocol.DecodeQueryReq(payload);
                return PipeProtocol.EncodeQueryReq(options, text);
            }
            case (PipeProtocol.Op.Query, true):
            {
                var (resultId, count, traceJson) = PipeProtocol.DecodeQueryResp(payload);
                return PipeProtocol.EncodeQueryResp(resultId, count, traceJson);
            }
            case (PipeProtocol.Op.ResultPage, false):
            {
                var (resultId, offset, count) = PipeProtocol.DecodeResultPageReq(payload);
                return PipeProtocol.EncodeResultPageReq(resultId, offset, count);
            }
            case (PipeProtocol.Op.ResultPage, true):
                return PipeProtocol.EncodePageResp(PipeProtocol.DecodePageResp(payload));
            case (PipeProtocol.Op.ResultFree, false):
                return PipeProtocol.EncodeResultFreeReq(
                    PipeProtocol.DecodeResultFreeReq(payload));
            case (PipeProtocol.Op.ServiceInfo, true):
            {
                // C# has no ServiceInfo encoder (the client only consumes it);
                // pin the snake_case key set and pass the payload through.
                using var doc = JsonDocument.Parse(payload);
                Assert.True(doc.RootElement.TryGetProperty("uptime_ms", out _), file);
                Assert.True(doc.RootElement.TryGetProperty("connections", out _), file);
                Assert.True(doc.RootElement.TryGetProperty("version", out _), file);
                return payload;
            }
            default:
                throw new Xunit.Sdk.XunitException(
                    $"{file}: opcode {header.Opcode} (response={header.IsResponse}) has no "
                    + "handler in GoldenCorpusTests — every corpus case must be covered");
        }
    }

    [Fact]
    public void PageRespRows_CarryWtf8AndExtremeValues_Faithfully()
    {
        var bytes = Load("result_page_resp_rows.bin");
        var rows = PipeProtocol.DecodePageResp(
            bytes.AsSpan(PipeProtocol.HeaderLen).ToArray());

        Assert.Equal(3, rows.Count);
        Assert.Equal("alpha.txt", rows[0].Name);
        Assert.Equal("C:\\", rows[0].ParentPath);
        Assert.Equal("省察.txt", rows[1].Name);
        Assert.Equal("C:\\メモ\\", rows[1].ParentPath);
        Assert.Equal(0x1_0000_0001UL, rows[1].Size); // u64 survives, not truncated
        Assert.Equal(-5, rows[1].Mtime);             // i64 signedness survives
        // Unpaired surrogate U+D800 must round-trip through the WTF-8 decode
        // into UTF-16 (that's the whole point of WTF-8 on this wire).
        Assert.Equal('\uD800', rows[2].Name[0]);
        Assert.EndsWith("tail.dat", rows[2].Name);
        Assert.True(rows[2].IsDirectory);
    }

    [Fact]
    public void StatsSnapshotJson_DeserializesIntoEngineStatsData()
    {
        var stats = JsonSerializer.Deserialize<EngineStatsData>(
            Load("stats_snapshot.json"), SnakeCase)!;

        var trace = Assert.Single(stats.RecentQueries);
        Assert.Equal("win ext:txt", trace.Query);
        Assert.Equal(81UL, trace.TotalUs);
        Assert.NotEqual(0UL, stats.P50Us);
        Assert.NotEqual(0UL, stats.P99Us);
        Assert.Equal(26UL, Assert.Single(stats.RecentUsn).ApplyUs);
        var index = Assert.Single(stats.Indexes);
        Assert.Equal("C:", index.Volume);
        Assert.Equal(41UL, index.Entries);
        Assert.Equal(58UL, index.ContentGeneration);
        Assert.Equal(61UL, stats.Counters.StatFetchFailures);
        Assert.Equal(67UL, stats.Counters.JournalRescans);
        Assert.Equal(74UL, stats.Counters.PipeConnectionsRejected);
        var error = Assert.Single(stats.RecentErrors);
        Assert.Equal("warn", error.Severity);
        Assert.Equal("C:", error.Volume);
        Assert.Equal(81UL, error.Seq);
    }

    [Fact]
    public void QueryTraceJson_DeserializesIntoQueryTraceData()
    {
        var trace = JsonSerializer.Deserialize<QueryTraceData>(
            Load("query_trace.json"), SnakeCase)!;

        Assert.Equal("win ext:txt", trace.Query);
        Assert.Equal("pool-scan", trace.Driver);
        Assert.False(trace.Unchanged);
        Assert.Equal(11UL, trace.ParseUs);
        Assert.Equal(12UL, trace.CompileUs);
        Assert.Equal(13UL, trace.MemoUs);
        Assert.Equal(14UL, trace.ScanUs);
        Assert.Equal(15UL, trace.MaterializeUs);
        Assert.Equal(16UL, trace.MergeUs);
        Assert.Equal(81UL, trace.TotalUs);
        Assert.Equal(1_268_560UL, trace.EntriesScanned);
        Assert.Equal(17UL, trace.ExcludedSkipped);
        Assert.Equal(18UL, trace.Hits);
        Assert.Equal(2U, trace.Volumes);
    }
}
