using FindMyFiles.Engine;
using Xunit.Abstractions;

namespace FindMyFiles.Tests.Contract;

/// <summary>
/// Contract run #1: <see cref="FakeEngineClient"/>, always on. The fake is
/// what `--fake-engine` UI tests build on, so its conformance (shared syntax
/// fixture, cancellation, BumpEpoch staleness) is load-bearing.
/// </summary>
public sealed class FakeClientContractTests(ITestOutputHelper output)
    : EngineClientContractTests(output)
{
    protected override Task<IEngineClient?> AcquireClientOrSkipAsync() =>
        Task.FromResult<IEngineClient?>(new FakeEngineClient());

    protected override string ValidQuery => "file";

    protected override Task<bool> TryForceStaleAsync(IEngineClient client)
    {
        ((FakeEngineClient)client).BumpEpoch();
        return Task.FromResult(true);
    }
}
