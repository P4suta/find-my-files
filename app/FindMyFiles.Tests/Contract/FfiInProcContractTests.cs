using FindMyFiles.Engine;
using Xunit.Abstractions;

namespace FindMyFiles.Tests.Contract;

/// <summary>
/// Contract run #4: <see cref="FfiEngineClient"/> over fmf_engine.dll, gated
/// on FMF_ADMIN_TESTS=1 (the same environment gate as the elevated Rust
/// suite — the run that loads the native DLL into the test host). Partial
/// conformance without volumes: no indexing is started, so valid queries hit
/// 0 rows and staleness (a structural rebuild) cannot be forced — that fact
/// skips. The engine gets a throwaway index dir, never %ProgramData%, so it
/// cannot collide with an installed service's writer lock.
/// </summary>
public sealed class FfiInProcContractTests(ITestOutputHelper output)
    : EngineClientContractTests(output)
{
    private readonly ITestOutputHelper _output = output;
    private string? _indexDir;

    protected override Task<IEngineClient?> AcquireClientOrSkipAsync()
    {
        if (!string.Equals(Environment.GetEnvironmentVariable("FMF_ADMIN_TESTS"), "1", StringComparison.Ordinal))
        {
            _output.WriteLine("FMF_ADMIN_TESTS != 1 — skipped (elevated gate)");
            return Task.FromResult<IEngineClient?>(null);
        }

        _indexDir = Path.Combine(Path.GetTempPath(), "fmf-ffitest-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(_indexDir);
        return Task.FromResult<IEngineClient?>(new FfiEngineClient(_indexDir));
    }

    protected override Task TeardownAsync()
    {
        if (_indexDir is not null)
        {
            try
            {
                Directory.Delete(_indexDir, recursive: true);
            }
            catch (IOException)
            {
                // Lock file released a beat late — temp dir is self-cleaning.
            }
        }

        return Task.CompletedTask;
    }

    protected override string ValidQuery => "anything";

    // No stale hook: a structural rebuild needs a real volume rescan.
}
