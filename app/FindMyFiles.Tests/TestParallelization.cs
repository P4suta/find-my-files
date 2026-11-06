using FindMyFiles.Tests;
using Xunit;
using Xunit.Sdk;
using Xunit.v3;

[assembly: Parallelization(Mode = ParallelMode.None)]
[assembly: TestCollectionOrderer<PipeConnectionContractOrderer>]
