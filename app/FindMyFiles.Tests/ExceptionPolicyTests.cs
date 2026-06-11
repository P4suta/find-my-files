using FindMyFiles.Services;
using Xunit;

namespace FindMyFiles.Tests;

public sealed class ExceptionPolicyTests
{
    /// <summary>Pins the XAML suppression rule: the first three unhandled
    /// exceptions are absorbed, the fourth declares a storm (crash marker +
    /// honest death).</summary>
    [Theory]
    [InlineData(1, true)]
    [InlineData(2, true)]
    [InlineData(3, true)]
    [InlineData(4, false)]
    public void Storm_budget_suppresses_exactly_the_first_three(int occurrence, bool suppressed) =>
        Assert.Equal(suppressed, ExceptionPolicy.WithinStormBudget(occurrence));
}
