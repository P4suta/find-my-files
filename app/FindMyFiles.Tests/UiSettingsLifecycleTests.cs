using System.Diagnostics;
using Xunit;

namespace FindMyFiles.Tests;

public sealed class UiSettingsLifecycleTests
{
    [Theory]
    [InlineData("delayed-close")]
    [InlineData("close-invoke-error")]
    [InlineData("close-wait-error")]
    [InlineData("close-ready-error")]
    [InlineData("open-invoke-error")]
    [InlineData("open-wait-error")]
    public async Task Settings_operations_wait_for_completion_and_preserve_failures(string scenario)
    {
        var directory = Path.Combine(AppContext.BaseDirectory, "UiAutomation");
        var start = new ProcessStartInfo("pwsh")
        {
            UseShellExecute = false,
            CreateNoWindow = true,
            RedirectStandardOutput = true,
            RedirectStandardError = true,
        };
        foreach (var argument in new[]
        {
            "-NoProfile", "-NonInteractive", "-File",
            Path.Combine(directory, "settings-lifecycle-tests.ps1"),
            "-TestScriptPath", Path.Combine(directory, "ui-tests.ps1"),
            "-Scenario", scenario,
        })
        {
            start.ArgumentList.Add(argument);
        }

        using var process = Process.Start(start) ?? throw new InvalidOperationException("PowerShell did not start.");
        var output = process.StandardOutput.ReadToEndAsync();
        var error = process.StandardError.ReadToEndAsync();
        try
        {
            await process.WaitForExitAsync().WaitAsync(TimeSpan.FromSeconds(30));
        }
        catch (TimeoutException)
        {
            process.Kill(entireProcessTree: true);
            await process.WaitForExitAsync();
            await Task.WhenAll(output, error);
            throw;
        }

        Assert.True(process.ExitCode == 0, $"{scenario}: {await output}\n{await error}");
    }
}
