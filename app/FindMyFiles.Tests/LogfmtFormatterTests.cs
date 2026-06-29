using System.Text;
using FindMyFiles.Services;
using Serilog;
using Serilog.Core;
using Serilog.Events;
using Xunit;

namespace FindMyFiles.Tests;

/// <summary>Behavioural tests for <see cref="LogfmtFormatter"/> — the quoting
/// rules (which are the log-injection defence) and the line shape, mirroring the
/// Rust engine's logfmt tests so both sides stay in lockstep (ADR-0037).</summary>
public sealed class LogfmtFormatterTests
{
    private static string Esc(string value)
    {
        var sb = new StringBuilder();
        LogfmtFormatter.AppendValue(sb, value);
        return sb.ToString();
    }

    [Theory]
    [InlineData("C:")]
    [InlineData("query-served_7f3a")]
    [InlineData("1240")]
    public void AppendValue_leaves_safe_values_bare(string value) =>
        Assert.Equal(value, Esc(value));

    [Theory]
    [InlineData("a b", "\"a b\"")]
    [InlineData("k=v", "\"k=v\"")]
    [InlineData("say \"hi\"", "\"say \\\"hi\\\"\"")]
    [InlineData("back\\slash", "\"back\\\\slash\"")]
    [InlineData("", "\"\"")]
    [InlineData("tab\there", "\"tab\\there\"")]
    public void AppendValue_quotes_and_escapes(string value, string expected) =>
        Assert.Equal(expected, Esc(value));

    [Fact]
    public void AppendValue_neutralises_log_injection()
    {
        // CR/LF fold onto one line so a crafted value cannot forge a record.
        var escaped = Esc("real\r\nFAKE level=ERROR msg=pwned");
        Assert.DoesNotContain('\n', escaped);
        Assert.DoesNotContain('\r', escaped);
        Assert.Contains("\\r\\n", escaped, StringComparison.Ordinal);

        // A bare control char becomes \u00XX.
        Assert.Equal("\"x\\u0007y\"", Esc("xy"));
    }

    [Fact]
    public void AppendValue_caps_long_values_with_a_marker()
    {
        var escaped = Esc(new string('a', 2000));
        Assert.EndsWith("…\"", escaped, StringComparison.Ordinal);
        Assert.True(escaped.Length < 2000);
    }

    [Fact]
    public void Line_orders_fields_with_msg_last_and_carries_inline_fields()
    {
        var line = Capture(LogEventLevel.Information, log => log
            .ForContext("area", "query")
            .ForContext("rid", 7UL)
            .ForContext("hits", 1240UL)
            .Information("{Msg}", "query served"));

        Assert.Contains(" INFO  area=query", line, StringComparison.Ordinal);
        Assert.Contains("hits=1240", line, StringComparison.Ordinal);
        Assert.Contains("rid=7", line, StringComparison.Ordinal);
        Assert.EndsWith(" msg=\"query served\"", line, StringComparison.Ordinal);

        // Inline fields are ordinal-sorted: hits before rid.
        Assert.True(
            line.IndexOf("hits=", StringComparison.Ordinal)
                < line.IndexOf("rid=", StringComparison.Ordinal),
            line);
    }

    [Fact]
    public void Line_folds_an_injected_newline_into_one_record()
    {
        var line = Capture(LogEventLevel.Information, log => log
            .ForContext("area", "x")
            .Information("{Msg}", "a\r\nFAKE level=ERROR"));

        Assert.DoesNotContain('\n', line);
        Assert.Contains("msg=\"a\\r\\nFAKE level=ERROR\"", line, StringComparison.Ordinal);
    }

    [Fact]
    public void Line_appends_the_exception_as_a_single_line_err_field()
    {
        var line = Capture(LogEventLevel.Information, log => log
            .ForContext("area", "x")
            .Error(new InvalidOperationException("kaboom"), "{Msg}", "boom"));

        Assert.Contains(" ERROR area=x", line, StringComparison.Ordinal);
        Assert.Contains(" err=", line, StringComparison.Ordinal);
        Assert.Contains("kaboom", line, StringComparison.Ordinal);
        Assert.DoesNotContain('\n', line);
    }

    [Fact]
    public void Line_maps_debug_level_tag()
    {
        var line = Capture(LogEventLevel.Debug, log => log
            .ForContext("area", "x")
            .Debug("{Msg}", "tick"));

        Assert.Contains(" DEBUG area=x", line, StringComparison.Ordinal);
    }

    /// <summary>Run one log call through a real Serilog pipeline using
    /// <see cref="LogfmtFormatter"/> and return the single rendered line (without
    /// its trailing newline).</summary>
    private static string Capture(LogEventLevel min, Action<ILogger> emit)
    {
        var sink = new CaptureSink();
        var logger = new LoggerConfiguration()
            .MinimumLevel.Is(min)
            .Enrich.FromLogContext()
            .WriteTo.Sink(sink)
            .CreateLogger();
        using (logger)
        {
            emit(logger);
        }

        return sink.Text.TrimEnd('\n');
    }

    private sealed class CaptureSink : ILogEventSink
    {
        private readonly LogfmtFormatter _formatter = new();
        private readonly StringWriter _writer = new();

        public string Text => _writer.ToString();

        public void Emit(LogEvent logEvent) => _formatter.Format(logEvent, _writer);
    }
}
