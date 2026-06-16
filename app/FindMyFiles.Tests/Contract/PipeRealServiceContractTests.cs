using System.Diagnostics;
using FindMyFiles.Engine;
using Xunit.Abstractions;
using static FindMyFiles.Tests.TestDoubles.Polling;

namespace FindMyFiles.Tests.Contract;

/// <summary>
/// Contract run #3: <see cref="PipeEngineClient"/> against the real
/// fmf-service, gated on FMF_PIPE_TESTS=1 (`just test-pipe` builds the
/// service binary first; PipeIntegrationTests の流儀). The service runs
/// unelevated, --no-index, throwaway data dir and unique pipe name — partial
/// conformance: queries parse for real (the golden syntax fixture hits the
/// real parser) but hit 0 rows, and there is no way to force staleness from
/// outside, so that fact skips.
/// </summary>
public sealed class PipeRealServiceContractTests(ITestOutputHelper output)
    : EngineClientContractTests(output)
{
    private readonly ITestOutputHelper _output = output;
    private Process? _service;
    private string? _dataDir;

    protected override async Task<IEngineClient?> AcquireClientOrSkipAsync()
    {
        if (!string.Equals(Environment.GetEnvironmentVariable("FMF_PIPE_TESTS"), "1", StringComparison.Ordinal))
        {
            _output.WriteLine("FMF_PIPE_TESTS != 1 — skipped (run via `just test-pipe`)");
            return null;
        }

        var exe = FindServiceExe();
        var pipeName = @"\\.\pipe\fmf-ctest-" + Guid.NewGuid().ToString("N");
        _dataDir = Path.Combine(Path.GetTempPath(), "fmf-ctest-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(_dataDir);
        _service = StartService(exe, pipeName, _dataDir);

        var client = new PipeEngineClient(pipeName);
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected,
            "connect to fmf-service",
            30_000);
        return client;
    }

    protected override Task TeardownAsync()
    {
        KillQuietly(_service);
        _service = null;
        if (_dataDir is not null)
        {
            TryDeleteDirectory(_dataDir);
        }

        return Task.CompletedTask;
    }

    protected override string ValidQuery => "anything";

    protected override bool IsInProcTransport => false;

    // No stale hook: forcing a structural rebuild needs volumes (elevation),
    // and killing the service is the integration test's job — the stale fact
    // skips for this derivation by design.

    /// <summary>Resolve build/engine/release/fmf-service.exe by walking up
    /// from the test assembly (repo-relative; built by `just service-build`).</summary>
    private static string FindServiceExe()
    {
        for (var dir = new DirectoryInfo(AppContext.BaseDirectory); dir is not null; dir = dir.Parent)
        {
            var candidate = Path.Combine(
                dir.FullName, "build", "engine", "release", "fmf-service.exe");
            if (File.Exists(candidate))
            {
                return candidate;
            }
        }

        throw new FileNotFoundException(
            "build/engine/release/fmf-service.exe not found above "
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
            _output.WriteLine($"[{tag}] {line}");
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
