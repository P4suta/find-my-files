using Xunit.Sdk;
using Xunit.v3;

namespace FindMyFiles.Tests;

public sealed class PipeConnectionContractOrderer : ITestCollectionOrderer
{
    public const string CollectionName = "Pipe connection contracts";

    public IReadOnlyCollection<TTestCollection> OrderTestCollections<TTestCollection>(
        IReadOnlyCollection<TTestCollection> testCollections)
        where TTestCollection : ITestCollection =>
        testCollections
            .OrderByDescending(collection => string.Equals(
                collection.TestCollectionDisplayName,
                CollectionName,
                StringComparison.Ordinal))
            .ToArray();
}
