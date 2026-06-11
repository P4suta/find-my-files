using System.Diagnostics;
using FindMyFiles.Engine;
using Xunit;
using Xunit.Abstractions;
using static FindMyFiles.Tests.TestDoubles.Polling;

namespace FindMyFiles.Tests;

/// <summary>
/// C# client × real fmf-service integration (docs/ARCHITECTURE.md pipe test
/// gates). Gated on FMF_PIPE_TESTS=1 (`just test-pipe`, which builds the
/// service first); without it the test reports and returns green so plain
/// `just test-app` stays hermetic. The service runs unelevated on a unique
/// pipe name with --no-index and a throwaway data dir — no real volumes, no
/// elevation.
/// </summary>
public sealed class PipeIntegrationTests(ITestOutputHelper output)
{
    private const string GateVariable = "FMF_PIPE_TESTS";

    [Fact]
    public async Task RealService_SearchAsync_KillAsync_ReconnectAsync_RoundTrips()
    {
        if (Environment.GetEnvironmentVariable(GateVariable) != "1")
        {
            output.WriteLine($"{GateVariable} != 1 — skipped (run via `just test-pipe`)");
            return;
        }
        var exe = FindServiceExe();
        var pipeName = @"\\.\pipe\fmf-itest-" + Guid.NewGuid().ToString("N");
        var dataDir = Path.Combine(Path.GetTempPath(), "fmf-itest-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(dataDir);

        Process? service = null;
        try
        {
            service = StartService(exe, pipeName, dataDir);
            using var client = new PipeEngineClient(pipeName);
            await WaitUntilAsync(
                () => client.Connection == EngineConnectionState.Connected,
                "connect to fmf-service", 30_000);

            // --no-index: zero Ready volumes. A query still succeeds (the
            // contract: queries always target Ready volumes only) with 0 hits.
            var outcome = await client.SearchAsync("anything", SearchOptions.Default);
            Assert.Equal(0, outcome.Result.Count);
            outcome.Result.Dispose();

            var stats = await client.GetStatsAsync();
            Assert.NotNull(stats);
            Assert.NotNull(stats!.Transport);

            Assert.Empty(await client.GetStatusAsync());

            // Kill the service: the next request fails fast, no 10s timeout.
            service.Kill();
            await service.WaitForExitAsync();
            await WaitUntilAsync(
                () => client.Connection == EngineConnectionState.Reconnecting,
                "client notices the dead service", 15_000);
            await Assert.ThrowsAsync<EngineUnavailableException>(
                () => client.SearchAsync("anything", SearchOptions.Default));

            // Restart on the same pipe + data dir (the OS released the
            // writer lock with the killed process): the supervisor reconnects
            // by itself and searching works again.
            service.Dispose();
            service = StartService(exe, pipeName, dataDir);
            await WaitUntilAsync(
                () => client.Connection == EngineConnectionState.Connected,
                "reconnect to the restarted service", 30_000);
            var again = await client.SearchAsync("anything", SearchOptions.Default);
            Assert.Equal(0, again.Result.Count);
            again.Result.Dispose();
        }
        finally
        {
            KillQuietly(service);
            TryDeleteDirectory(dataDir);
        }
    }

    /// <summary>Resolve engine/target/release/fmf-service.exe by walking up
    /// from the test assembly (repo-relative; built by `just service-build`).</summary>
    private static string FindServiceExe()
    {
        for (var dir = new DirectoryInfo(AppContext.BaseDirectory); dir is not null; dir = dir.Parent)
        {
            var candidate = Path.Combine(
                dir.FullName, "engine", "target", "release", "fmf-service.exe");
            if (File.Exists(candidate))
            {
                return candidate;
            }
        }
        throw new FileNotFoundException(
            "engine/target/release/fmf-service.exe not found above "
            + $"{AppContext.BaseDirectory} — run `just service-build` first");
    }

    private Process StartService(string exe, string pipeName, string dataDir)
    {
        var psi = new ProcessStartInfo
        {
            FileName = exe,
            UseShellExecute = false,
            CreateNoWindow = true,
            RedirectStandardOutput = true,
            RedirectStandardError = true,
        };
        psi.ArgumentList.Add("run");
        psi.ArgumentList.Add("--pipe-name");
        psi.ArgumentList.Add(pipeName);
        psi.ArgumentList.Add("--data-dir");
        psi.ArgumentList.Add(dataDir);
        psi.ArgumentList.Add("--no-index");

        var p = Process.Start(psi)
            ?? throw new InvalidOperationException($"failed to start {exe}");
        p.OutputDataReceived += (_, e) => LogQuietly("svc", e.Data);
        p.ErrorDataReceived += (_, e) => LogQuietly("svc!", e.Data);
        p.BeginOutputReadLine();
        p.BeginErrorReadLine();
        return p;
    }

    /// <summary>Child output may trickle in after the test finished, when
    /// ITestOutputHelper throws — drop those lines instead of crashing.</summary>
    private void LogQuietly(string tag, string? line)
    {
        if (line is null)
        {
            return;
        }
        try
        {
            output.WriteLine($"[{tag}] {line}");
        }
        catch (InvalidOperationException)
        {
        }
    }

    private static void KillQuietly(Process? p)
    {
        if (p is null)
        {
            return;
        }
        try
        {
            if (!p.HasExited)
            {
                p.Kill();
                p.WaitForExit(5000);
            }
        }
        catch
        {
            // Already gone — nothing to clean.
        }
        finally
        {
            p.Dispose();
        }
    }

    /// <summary>The killed service may release file handles a beat late —
    /// retry briefly, then give up (the OS temp dir is self-cleaning).</summary>
    private static void TryDeleteDirectory(string dir)
    {
        for (var attempt = 0; attempt < 5; attempt++)
        {
            try
            {
                Directory.Delete(dir, recursive: true);
                return;
            }
            catch (DirectoryNotFoundException)
            {
                return;
            }
            catch (IOException)
            {
                Thread.Sleep(200);
            }
            catch (UnauthorizedAccessException)
            {
                Thread.Sleep(200);
            }
        }
    }
}
