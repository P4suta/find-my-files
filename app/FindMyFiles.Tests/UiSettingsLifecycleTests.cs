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
    public Task Settings_operations_wait_for_completion_and_preserve_failures(string scenario) =>
        RunScenarioAsync(scenario, TestContext.Current.CancellationToken);

    [Fact]
    public async Task Cancellation_reaps_its_owned_child()
    {
        using var cancellation = CancellationTokenSource.CreateLinkedTokenSource(TestContext.Current.CancellationToken);
        Process? observer = null;
        try
        {
            await Assert.ThrowsAnyAsync<OperationCanceledException>(() =>
                RunScenarioAsync("wait-for-cancel", cancellation.Token, process =>
                {
                    observer = Process.GetProcessById(process.Id);
                    _ = observer.SafeHandle;
                    cancellation.Cancel();
                }));
            Assert.NotNull(observer);
            Assert.True(observer.HasExited);
        }
        finally
        {
            if (observer is not null)
            {
                try
                {
                    if (!observer.HasExited)
                    {
                        observer.Kill(entireProcessTree: true);
                        await observer.WaitForExitAsync(CancellationToken.None);
                    }
                }
                finally
                {
                    observer.Dispose();
                }
            }
        }
    }

    private static async Task RunScenarioAsync(string scenario, CancellationToken token, Action<Process>? started = null)
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
        started?.Invoke(process);
        using var cancellation = CancellationTokenSource.CreateLinkedTokenSource(token);
        cancellation.CancelAfter(TimeSpan.FromSeconds(30));
        var output = process.StandardOutput.ReadToEndAsync(cancellation.Token);
        var error = process.StandardError.ReadToEndAsync(cancellation.Token);
        try
        {
            await process.WaitForExitAsync(cancellation.Token);
        }
        catch (OperationCanceledException)
        {
            try
            {
                process.Kill(entireProcessTree: true);
            }
            catch (InvalidOperationException) when (process.HasExited)
            {
            }

            await process.WaitForExitAsync(CancellationToken.None);
            try
            {
                await Task.WhenAll(output, error);
            }
            catch (OperationCanceledException) when (cancellation.IsCancellationRequested)
            {
            }

            throw;
        }

        Assert.True(process.ExitCode == 0, $"{scenario}: {await output}\n{await error}");
    }
}
