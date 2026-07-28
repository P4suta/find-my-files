using FindMyFiles.Services;
using Xunit;

namespace FindMyFiles.Tests;

public sealed class LogSetupTests
{
    [Fact]
    public void CreateLogger_creates_the_directory_and_writes_logfmt()
    {
        var root = Directory.CreateTempSubdirectory("fmf-log-setup-");
        var logDir = Path.Combine(root.FullName, "nested", "logs");
        try
        {
            using (var logger = LogSetup.CreateLogger(logDir))
            {
                logger.Information("ready");
            }

            var contents = File.ReadAllText(Path.Combine(logDir, "app.log"));
            Assert.Contains("msg=ready", contents, StringComparison.Ordinal);
        }
        finally
        {
            root.Delete(recursive: true);
        }
    }

    [Fact]
    public void CreateLogger_storage_failure_degrades_without_leaking_the_path()
    {
        const string secret = "secret-query";
        string? report = null;

        using var logger = LogSetup.CreateLogger(
            $"{secret}\0invalid",
            value => report = value);
        logger.Information("app remains usable");

        Assert.NotNull(report);
        Assert.Contains("log initialization failed", report, StringComparison.Ordinal);
        Assert.DoesNotContain(secret, report, StringComparison.Ordinal);
    }
}
