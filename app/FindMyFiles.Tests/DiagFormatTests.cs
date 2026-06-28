using FindMyFiles.Converters;
using FindMyFiles.Engine;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Xunit;

namespace FindMyFiles.Tests;

/// <summary>
/// Unit tests for the F12 diagnostics formatters. These pin the invariant-culture
/// rendering the panel binds to (the logic previously lived untested in the
/// panel's code-behind), including the ADR-0018 guard that a contract counter
/// surfaces by reflection with no UI edits.
/// </summary>
public sealed class DiagFormatTests
{
    [Theory]
    [InlineData(0UL, "0")]
    [InlineData(1000UL, "1,000")]
    [InlineData(812003UL, "812,003")]
    public void Count_groups_with_invariant_separators(ulong n, string expected) =>
        Assert.Equal(expected, DiagFormat.Count(n));

    [Theory]
    [InlineData(0UL, "0 MB")]
    [InlineData(95420416UL, "91 MB")] // 91 * 1024 * 1024
    public void Mb_rounds_to_whole_mebibytes(ulong bytes, string expected) =>
        Assert.Equal(expected, DiagFormat.Mb(bytes));

    [Theory]
    [InlineData(0UL, "0.00 ms")]
    [InlineData(1000UL, "1.00 ms")]
    [InlineData(2340UL, "2.34 ms")]
    public void Ms_renders_micros_as_millis(ulong micros, string expected) =>
        Assert.Equal(expected, DiagFormat.Ms(micros));

    [Theory]
    [InlineData(0.0, "0 B/件")]
    [InlineData(110.0, "110 B/件")]
    public void BytesPer_rounds_with_unit(double v, string expected) =>
        Assert.Equal(expected, DiagFormat.BytesPer(v));

    [Theory]
    [InlineData(0.0, "0 µs")]
    [InlineData(60.4, "60 µs")]
    public void Micros_rounds_with_unit(double v, string expected) =>
        Assert.Equal(expected, DiagFormat.Micros(v));

    [Fact]
    public void Gen_labels_the_generation() => Assert.Equal("gen 7", DiagFormat.Gen(7));

    [Fact]
    public void Query_is_empty_when_no_trace() =>
        Assert.Equal(string.Empty, DiagFormat.Query(null));

    [Fact]
    public void Query_renders_the_empty_query_as_all() =>
        Assert.Equal("(all)", DiagFormat.Query(new QueryTraceData { Query = string.Empty }));

    [Fact]
    public void Query_passes_through_the_query_text() =>
        Assert.Equal("report", DiagFormat.Query(new QueryTraceData { Query = "report" }));

    [Fact]
    public void Stat_tiles_are_empty_when_no_trace()
    {
        Assert.Equal(string.Empty, DiagFormat.TotalMs(null));
        Assert.Equal(string.Empty, DiagFormat.Hits(null));
        Assert.Equal(string.Empty, DiagFormat.Scanned(null));
        Assert.Equal(string.Empty, DiagFormat.Driver(null));
    }

    [Fact]
    public void Stat_tiles_format_their_trace_fields()
    {
        var trace = new QueryTraceData
        {
            TotalUs = 6050,
            Hits = 1234,
            EntriesScanned = 100000,
            Driver = "suffix",
        };
        Assert.Equal("6.05 ms", DiagFormat.TotalMs(trace));
        Assert.Equal("1,234", DiagFormat.Hits(trace));
        Assert.Equal("100,000", DiagFormat.Scanned(trace));
        Assert.Equal("suffix", DiagFormat.Driver(trace));
    }

    [Fact]
    public void Percentiles_is_empty_when_no_stats() =>
        Assert.Equal(string.Empty, DiagFormat.Percentiles(null));

    [Fact]
    public void Percentiles_renders_the_standard_p50_p90_p99_p999_line()
    {
        var stats = new EngineStatsData { P50Us = 1200, P90Us = 2100, P99Us = 4500, P999Us = 9000 };
        Assert.Equal(
            "p50 1.20 ms · p90 2.10 ms · p99 4.50 ms · p99.9 9.00 ms",
            DiagFormat.Percentiles(stats));
    }

    [Fact]
    public void ProcessMemory_is_empty_when_no_stats() =>
        Assert.Equal(string.Empty, DiagFormat.ProcessMemory(null));

    [Fact]
    public void ProcessMemory_renders_private_bytes_and_working_set_with_standard_names()
    {
        // 142 MiB private, 138 MiB working set.
        var stats = new EngineStatsData
        {
            CurrentPrivateBytes = 142UL * 1024 * 1024,
            CurrentWsBytes = 138UL * 1024 * 1024,
        };
        Assert.Equal("Private Bytes 142 MB · Working Set 138 MB", DiagFormat.ProcessMemory(stats));
    }

    [Fact]
    public void IndexTotal_sums_resident_index_bytes_across_volumes()
    {
        var stats = new EngineStatsData
        {
            Indexes =
            [
                new IndexStatsData { TotalBytes = 40UL * 1024 * 1024 },
                new IndexStatsData { TotalBytes = 24UL * 1024 * 1024 },
            ],
        };
        Assert.Equal("64 MB", DiagFormat.IndexTotal(stats));
    }

    [Fact]
    public void Overhead_is_working_set_minus_index_bytes_clamped_at_zero()
    {
        var stats = new EngineStatsData
        {
            CurrentWsBytes = 100UL * 1024 * 1024,
            Indexes = [new IndexStatsData { TotalBytes = 70UL * 1024 * 1024 }],
        };
        Assert.Equal("30 MB", DiagFormat.Overhead(stats));

        // Index larger than WS (shouldn't happen, but must not underflow).
        var weird = new EngineStatsData
        {
            CurrentWsBytes = 10UL * 1024 * 1024,
            Indexes = [new IndexStatsData { TotalBytes = 70UL * 1024 * 1024 }],
        };
        Assert.Equal("0 MB", DiagFormat.Overhead(weird));
    }

    [Fact]
    public void IndexColumns_breaks_down_the_dominant_columns_with_garbage_ratio() =>
        Assert.Equal(
            "name 40 MB · lower 40 MB · offsets 8 MB · parent 4 MB · perm 4 MB · frn-map 8 MB · dead 1 MB (1.5%)",
            DiagFormat.IndexColumns(
                40UL * 1024 * 1024,
                40UL * 1024 * 1024,
                8UL * 1024 * 1024,
                4UL * 1024 * 1024,
                4UL * 1024 * 1024,
                8UL * 1024 * 1024,
                1UL * 1024 * 1024,
                0.015));

    [Fact]
    public void Scan_formats_a_restore_line_with_phase_breakdown_and_peak() =>
        Assert.Equal(
            "C: snapshot 1.2 s · 850 MB/s · parse 34ms build 36ms sort 37ms · peak 410 MB · 1,234,567件",
            DiagFormat.Scan("C:", "snapshot", 850, 34, 36, 37, 1200, 1_234_567, 410UL * 1024 * 1024));

    [Fact]
    public void Service_is_empty_for_inproc_clients() =>
        Assert.Equal(string.Empty, DiagFormat.Service(new EngineStatsData { Service = null }));

    [Fact]
    public void Service_renders_uptime_connections_and_version()
    {
        var stats = new EngineStatsData
        {
            Service = new ServiceInfoData { UptimeMs = 8_000_000, Connections = 1, Version = "0.1.0" },
        };
        Assert.Equal("uptime 2h13m · 接続 1 · v0.1.0", DiagFormat.Service(stats));
    }

    [Fact]
    public void ServiceVis_tracks_presence_of_service_info()
    {
        Assert.Equal(Visibility.Collapsed, DiagFormat.ServiceVis(new EngineStatsData { Service = null }));
        Assert.Equal(
            Visibility.Visible,
            DiagFormat.ServiceVis(new EngineStatsData { Service = new ServiceInfoData() }));
    }

    [Fact]
    public void Transport_is_empty_for_inproc_clients() =>
        Assert.Equal(string.Empty, DiagFormat.Transport(new EngineStatsData { Transport = null }));

    [Fact]
    public void Transport_renders_pipe_metrics()
    {
        var stats = new EngineStatsData
        {
            Transport = new TransportStatsData
            {
                State = "Connected",
                Reconnects = 2,
                PageRttEwmaUs = 60,
                ServerPid = 4820,
            },
        };
        Assert.Equal("Connected · reconnects 2 · RTT 60 µs · pid 4820", DiagFormat.Transport(stats));
    }

    [Fact]
    public void Usn_formats_one_batch_line() =>
        Assert.Equal("C: 5rec → +3 -1 ~1 (60 µs)", DiagFormat.Usn("C:", 5, 3, 1, 1, 60));

    [Fact]
    public void Error_includes_uptime_severity_area_volume_and_first_line() =>
        Assert.Equal("[12s] ERROR usn (C:): x", DiagFormat.Error(12000, "error", "usn", "C:", "x\ny"));

    [Fact]
    public void Error_omits_the_volume_when_not_scoped() =>
        Assert.Equal("[12s] WARN core: msg", DiagFormat.Error(12000, "warn", "core", null, "msg"));

    [Fact]
    public void Counters_is_empty_when_null_or_all_zero()
    {
        Assert.Equal(string.Empty, DiagFormat.Counters(null));
        Assert.Equal(string.Empty, DiagFormat.Counters(new EngineStatsData()));
    }

    [Fact]
    public void Counters_surfaces_a_nonzero_contract_counter_by_snake_case_name()
    {
        // ADR-0018 guard: a counter set on the generated CountersData must
        // appear via reflection with its snake_case contract name.
        var stats = new EngineStatsData { Counters = new CountersData { WalkReadErrors = 3 } };
        Assert.Equal("劣化: walk_read_errors=3", DiagFormat.Counters(stats));
    }

    [Fact]
    public void HealthyVis_is_visible_only_when_clean()
    {
        Assert.Equal(Visibility.Collapsed, DiagFormat.HealthyVis(null));
        Assert.Equal(Visibility.Visible, DiagFormat.HealthyVis(new EngineStatsData()));
        Assert.Equal(
            Visibility.Collapsed,
            DiagFormat.HealthyVis(new EngineStatsData { Counters = new CountersData { WalkReadErrors = 1 } }));
        Assert.Equal(
            Visibility.Collapsed,
            DiagFormat.HealthyVis(new EngineStatsData { RecentErrors = [new ErrorEventData()] }));
    }

    [Fact]
    public void CountersVis_is_visible_only_when_degraded()
    {
        Assert.Equal(Visibility.Collapsed, DiagFormat.CountersVis(null));
        Assert.Equal(Visibility.Collapsed, DiagFormat.CountersVis(new EngineStatsData()));
        Assert.Equal(
            Visibility.Visible,
            DiagFormat.CountersVis(new EngineStatsData { Counters = new CountersData { WalkReadErrors = 1 } }));
    }

    [Fact]
    public void HasErrorsVis_is_visible_only_when_errors_present()
    {
        Assert.Equal(Visibility.Collapsed, DiagFormat.HasErrorsVis(null));
        Assert.Equal(Visibility.Collapsed, DiagFormat.HasErrorsVis(new EngineStatsData()));
        Assert.Equal(
            Visibility.Visible,
            DiagFormat.HasErrorsVis(new EngineStatsData { RecentErrors = [new ErrorEventData()] }));
    }

    [Fact]
    public void HealthSeverity_is_informational_before_the_first_poll() =>
        Assert.Equal(InfoBarSeverity.Informational, DiagFormat.HealthSeverity(null));

    [Fact]
    public void HealthSeverity_is_success_when_clean() =>
        Assert.Equal(InfoBarSeverity.Success, DiagFormat.HealthSeverity(new EngineStatsData()));

    [Fact]
    public void HealthSeverity_is_warning_when_only_counters_degraded() =>
        Assert.Equal(
            InfoBarSeverity.Warning,
            DiagFormat.HealthSeverity(new EngineStatsData { Counters = new CountersData { WalkReadErrors = 1 } }));

    [Fact]
    public void HealthSeverity_is_error_when_recent_errors_present() =>
        Assert.Equal(
            InfoBarSeverity.Error,
            DiagFormat.HealthSeverity(new EngineStatsData { RecentErrors = [new ErrorEventData()] }));

    [Fact]
    public void HealthSeverity_prefers_error_over_warning() =>
        Assert.Equal(
            InfoBarSeverity.Error,
            DiagFormat.HealthSeverity(new EngineStatsData
            {
                RecentErrors = [new ErrorEventData()],
                Counters = new CountersData { WalkReadErrors = 1 },
            }));
}
