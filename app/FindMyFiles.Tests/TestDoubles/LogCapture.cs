using FindMyFiles.Services;
using Serilog;
using Serilog.Core;
using Serilog.Events;

namespace FindMyFiles.Tests.TestDoubles;

/// <summary>Process-wide Serilog capture scope for serial behavioral tests.</summary>
internal sealed class LogCapture : IDisposable
{
    private readonly ILogger _previous = Log.Logger;
    private readonly CaptureSink _sink = new();
    private readonly ILogger _logger;

    public LogCapture()
    {
        _logger = new LoggerConfiguration()
            .MinimumLevel.Debug()
            .Enrich.FromLogContext()
            .WriteTo.Sink(_sink)
            .CreateLogger();
        Log.Logger = _logger;
    }

    public string Text => _sink.Text;

    public void Dispose()
    {
        Log.Logger = _previous;
        (_logger as IDisposable)?.Dispose();
    }

    private sealed class CaptureSink : ILogEventSink
    {
        private readonly LogfmtFormatter _formatter = new();
        private readonly StringWriter _writer = new();

        public string Text => _writer.ToString();

        public void Emit(LogEvent logEvent) => _formatter.Format(logEvent, _writer);
    }
}
