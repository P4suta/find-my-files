using System.Collections;
using System.Reflection;
using System.Text.Json;
using FindMyFiles.Engine;
using FindMyFiles.Services;
using Xunit;

namespace FindMyFiles.Tests;

public sealed class DiagnosticSanitizerTests
{
    private const string Secret = "C:\\Users\\alice\\secret-query.txt";

    /// <summary>
    /// Every string position in the stats snapshot, classified. This is the
    /// tripwire behind the whole privacy boundary: it is compared against the
    /// contract types by reflection below, so a string field added anywhere in
    /// the snapshot fails the build until somebody puts it in one of these two
    /// lists. That is what stops a new field from inheriting an existing
    /// field's review by reusing its name at a new depth.
    /// </summary>
    private static readonly string[] ReviewedPositions =
    [
        "recent_queries[]/driver",
        "recent_queries[]/cache",
        "recent_usn[]/volume",
        "scans[]/volume",
        "scans[]/source",
        "indexes[]/volume",
        "recent_errors[]/severity",
        "recent_errors[]/area",
        "recent_errors[]/volume",
        "transport/state",
        "service/version",
    ];

    /// <summary>Positions reviewed and deliberately kept redacted. Ring
    /// messages are free prose that demonstrably carries file-system paths
    /// (diag.rs records "cannot create log dir {dir}: {e}" into this ring).</summary>
    private static readonly string[] WithheldPositions =
    [
        "recent_errors[]/message",
    ];

    [Fact]
    public void SerializeStats_keeps_reviewed_labels_and_drops_user_data()
    {
        var stats = new EngineStatsData
        {
            P99Us = 1234,
            RecentQueries = [new QueryTraceData { Driver = "full-scan", Cache = "refine", QueryLength = 7 }],
            Indexes = [new IndexStatsData { Volume = "C:", Entries = 42 }],
            RecentErrors = [new ErrorEventData { Message = Secret, Area = "scan", Volume = "D:", Severity = "warn" }],
            Service = new ServiceInfoData { Version = "0.1.0-nightly.20260629+g3672e3f" },
            Transport = new TransportStatsData { State = "Connected" },
        };

        var json = DiagnosticSanitizer.SerializeStats(stats);

        // The half that was missing: reviewed labels survive, so the numbers
        // have something to attach to.
        Assert.Equal("full-scan", Text(json, "recent_queries[]/driver"));
        Assert.Equal("refine", Text(json, "recent_queries[]/cache"));
        Assert.Equal("C:", Text(json, "indexes[]/volume"));
        Assert.Equal("D:", Text(json, "recent_errors[]/volume"));
        Assert.Equal("scan", Text(json, "recent_errors[]/area"));
        Assert.Equal("warn", Text(json, "recent_errors[]/severity"));
        Assert.Equal("Connected", Text(json, "transport/state"));
        Assert.Equal("0.1.0-nightly.20260629+g3672e3f", Text(json, "service/version"));

        // The half that was already right: user data never crosses.
        Assert.DoesNotContain(Secret, json, StringComparison.Ordinal);
        Assert.Equal("[redacted:unknown-field]", Text(json, "recent_errors[]/message"));

        // Numeric evidence is unaffected.
        Assert.Contains("\"p99_us\": 1234", json, StringComparison.Ordinal);
        Assert.Contains("\"query_length\": 7", json, StringComparison.Ordinal);
        Assert.Contains("\"entries\": 42", json, StringComparison.Ordinal);
    }

    [Fact]
    public void SerializeStats_withholds_a_reviewed_position_whose_value_is_not_a_tag()
    {
        // `driver` is reviewed, but a value that could not have come from the
        // engine's closed label set is still withheld — and says which of the
        // two failures it was, because the fixes are opposite (widen the
        // allowlist vs. correct the producer).
        var stats = new EngineStatsData
        {
            RecentQueries = [new QueryTraceData { Driver = Secret }],
            Indexes = [new IndexStatsData { Volume = new string('C', 65) }],
        };

        var json = DiagnosticSanitizer.SerializeStats(stats);

        Assert.DoesNotContain(Secret, json, StringComparison.Ordinal);
        Assert.Equal("[redacted:unsafe-value]", Text(json, "recent_queries[]/driver"));
        Assert.Equal("[redacted:unsafe-value]", Text(json, "indexes[]/volume"));
    }

    [Fact]
    public void Only_the_build_version_position_accepts_a_plus()
    {
        // `+` is semver build metadata and belongs to exactly one position;
        // widening the default shape would let it through everywhere.
        var stats = new EngineStatsData
        {
            RecentQueries = [new QueryTraceData { Driver = "full+scan" }],
            Service = new ServiceInfoData { Version = "0.1.0-dev+g3672e3f" },
        };

        var json = DiagnosticSanitizer.SerializeStats(stats);

        Assert.Equal("[redacted:unsafe-value]", Text(json, "recent_queries[]/driver"));
        Assert.Equal("0.1.0-dev+g3672e3f", Text(json, "service/version"));
    }

    /// <summary>Read one string out of the sanitized dump by the same path
    /// vocabulary the sanitizer matches on, so the assertions survive JSON
    /// escaping (System.Text.Json writes <c>+</c> as <c>+</c>).</summary>
    private static string? Text(string json, string path)
    {
        using var doc = JsonDocument.Parse(json);
        var element = doc.RootElement;
        foreach (var segment in path.Split('/'))
        {
            var name = segment;
            var indexed = name.EndsWith("[]", StringComparison.Ordinal);
            if (indexed)
            {
                name = name[..^2];
            }

            element = element.GetProperty(name);
            if (indexed)
            {
                element = element[0];
            }
        }

        return element.GetString();
    }

    [Fact]
    public void A_new_contract_string_field_is_not_reviewed_by_default()
    {
        // The tripwire. Walk the snapshot's type graph (not an instance — an
        // instance only shows the fields the fixture happened to populate) and
        // require every string position to be explicitly classified. A field
        // added to the contract lands here, redacted by SerializeStats' own
        // fail-closed default, and fails this test until it is reviewed.
        var actual = StringPositions(typeof(EngineStatsData), string.Empty, [])
            .Order(StringComparer.Ordinal)
            .ToArray();
        var classified = ReviewedPositions.Concat(WithheldPositions)
            .Order(StringComparer.Ordinal)
            .ToArray();

        Assert.Equal(classified, actual);

        // Non-vacuity: the walk must actually find the snapshot's strings, or
        // the comparison above would pass against an empty tree.
        Assert.NotEmpty(actual);

        // And the allowlist may only name positions that exist — a typo would
        // otherwise sit there matching nothing while its field stays redacted.
        Assert.Equal(
            ReviewedPositions.Order(StringComparer.Ordinal),
            DiagnosticSanitizer.Allowlist.Keys.Order(StringComparer.Ordinal));
    }

    /// <summary>Every string-valued position reachable from <paramref name="type"/>,
    /// in the same path vocabulary the sanitizer matches on.</summary>
    private static IEnumerable<string> StringPositions(Type type, string path, HashSet<Type> seen)
    {
        if (!seen.Add(type))
        {
            yield break; // a self-referential contract type would not terminate
        }

        foreach (var property in type.GetProperties(BindingFlags.Public | BindingFlags.Instance))
        {
            var name = JsonNamingPolicy.SnakeCaseLower.ConvertName(property.Name);
            var child = path.Length == 0 ? name : path + "/" + name;
            var propertyType = property.PropertyType;

            if (propertyType == typeof(string))
            {
                yield return child;
                continue;
            }

            // A List<T>: descend into the element type at "<path>[]".
            if (typeof(IEnumerable).IsAssignableFrom(propertyType)
                && propertyType.IsGenericType)
            {
                var element = propertyType.GetGenericArguments()[0];
                if (element == typeof(string))
                {
                    yield return child + "[]";
                }
                else if (element.IsClass)
                {
                    foreach (var nested in StringPositions(element, child + "[]", seen))
                    {
                        yield return nested;
                    }
                }

                continue;
            }

            if (propertyType.IsClass)
            {
                foreach (var nested in StringPositions(propertyType, child, seen))
                {
                    yield return nested;
                }
            }
        }

        seen.Remove(type);
    }
}
