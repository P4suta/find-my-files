using System.Collections.Frozen;
using System.Text.Json;
using System.Text.Json.Nodes;
using FindMyFiles.Engine;

namespace FindMyFiles.Services;

/// <summary>
/// Privacy boundary for the engine snapshot copied out of the process by the
/// F12 panel's one-click bug report.
/// <para>The policy is the one fmf-core's <c>diag.rs</c> already states for the
/// engine log: a <b>reviewed allowlist</b> of positions that structurally
/// cannot carry a file name, a query or other user data, each value checked
/// against the same finite-tag shape — and everything else redacted,
/// fail-closed, so a string field added to the contract stays withheld until
/// somebody reviews it.</para>
/// <para>This half used to implement only the fail-closed clause: every string
/// became <c>[redacted]</c>. That is safe and nearly useless — it also took the
/// volume letters, the query driver, the service version and the transport
/// state, leaving a bug report that carried numbers and nothing to attach them
/// to. Naming the safe positions is what turns fail-closed into a policy rather
/// than a refusal to say anything.</para>
/// <para>Positions are matched by <b>path</b>, not by name
/// (<c>recent_errors[]/area</c>, not <c>area</c>), so a future contract field
/// cannot inherit another field's review by reusing its name at a new depth.
/// <c>DiagnosticSanitizerTests</c> pins the complete set of string positions in
/// the snapshot, so adding one fails the build until it is classified.</para>
/// </summary>
internal static class DiagnosticSanitizer
{
    /// <summary>Marker for a string whose position is absent from
    /// <see cref="Allowlist"/> — nobody has reviewed it, so it is withheld.
    /// Same spelling as fmf-core's <c>diag::REDACTED_UNKNOWN_FIELD</c>: the two
    /// halves of one policy must read identically in a bug report, and an
    /// xtask guard pins them equal.</summary>
    internal const string RedactedUnknownField = "[redacted:unknown-field]";

    /// <summary>Marker for a reviewed position whose value is not a finite tag
    /// — the value-side failure, which needs the opposite fix (correct the
    /// producer, not the allowlist). Mirrors fmf-core's
    /// <c>diag::REDACTED_UNSAFE_VALUE</c>.</summary>
    internal const string RedactedUnsafeValue = "[redacted:unsafe-value]";

    /// <summary>Longest value any reviewed position may carry, mirroring
    /// fmf-core's <c>safe_diagnostic_tag</c>.</summary>
    private const int TagLengthCap = 64;

    /// <summary>The value shapes a reviewed position is allowed to carry.</summary>
    internal enum ValueShape
    {
        /// <summary>fmf-core's <c>safe_diagnostic_tag</c> exactly: 1..=64 bytes
        /// of <c>[A-Za-z0-9._:-]</c>. The default for every reviewed position.</summary>
        FiniteTag,

        /// <summary><see cref="FiniteTag"/> plus <c>+</c>, the semver
        /// build-metadata separator. Needed by exactly one position — the
        /// service's channel-aware build identity, e.g.
        /// <c>0.1.0-nightly.20260629+g3672e3f</c> — and deliberately not widened
        /// into the default, which would let a <c>+</c> through everywhere.</summary>
        BuildVersion,
    }

    /// <summary>
    /// The reviewed positions. Each one is here because the value is produced
    /// by the engine from a closed set (an execution-strategy name, a cache
    /// verdict, a severity, a drive letter, a state name, a compiled-in
    /// version) and therefore cannot carry a path, a file name or query text.
    /// <para>Deliberately absent, and the reason the default has to be
    /// redaction: <c>recent_errors[]/message</c>. Ring messages are free prose
    /// and demonstrably carry file-system paths — <c>diag.rs</c>'s own
    /// <c>init_logging</c> records <c>"cannot create log dir {dir}: {e}"</c>
    /// into that very ring.</para>
    /// </summary>
    internal static readonly FrozenDictionary<string, ValueShape> Allowlist =
        new Dictionary<string, ValueShape>(StringComparer.Ordinal)
        {
            // Execution strategy and cache verdict: engine-chosen labels from a
            // closed set ("full-scan", "suffix", "miss", "refine"). The query
            // text is never in the trace at all — only QueryLength.
            ["recent_queries[]/driver"] = ValueShape.FiniteTag,
            ["recent_queries[]/cache"] = ValueShape.FiniteTag,

            // Drive letters ("C:"). A volume label is the coarsest possible
            // locator and is needed to read every other number in the dump.
            ["recent_usn[]/volume"] = ValueShape.FiniteTag,
            ["scans[]/volume"] = ValueShape.FiniteTag,
            ["indexes[]/volume"] = ValueShape.FiniteTag,
            ["recent_errors[]/volume"] = ValueShape.FiniteTag,

            // How the index was established: "scan" | "snapshot".
            ["scans[]/source"] = ValueShape.FiniteTag,

            // Severity is fmf-core's Severity enum ("warn"|"error"|"panic");
            // area is the module path or logical tag that diag.rs itself
            // allowlists as `area` on the log line.
            ["recent_errors[]/severity"] = ValueShape.FiniteTag,
            ["recent_errors[]/area"] = ValueShape.FiniteTag,

            // EngineConnectionState.ToString() — an enum name.
            ["transport/state"] = ValueShape.FiniteTag,

            // The service binary's compiled-in build identity. Without it a
            // report cannot say which engine produced the numbers, and in pipe
            // mode the service *is* the engine (the app's own version travels
            // outside this JSON, in the DiagnosticCopy header).
            ["service/version"] = ValueShape.BuildVersion,
        }.ToFrozenDictionary(StringComparer.Ordinal);

    internal static string SerializeStats(EngineStatsData stats)
    {
        ArgumentNullException.ThrowIfNull(stats);
        var node = JsonSerializer.SerializeToNode(stats, EngineJson.SnakeCase);
        RedactStrings(node, string.Empty);
        return node?.ToJsonString(IndentedJson) ?? "null";
    }

    /// <summary>Replace every string in the tree with its sanitized form,
    /// carrying the position down so the decision is made per path.</summary>
    /// <param name="node">Subtree to sanitize in place.</param>
    /// <param name="path">Position of <paramref name="node"/>: property names
    /// joined by <c>/</c>, with <c>[]</c> appended when descending into an
    /// array's elements.</param>
    private static void RedactStrings(JsonNode? node, string path)
    {
        switch (node)
        {
            case JsonObject obj:
                foreach (var property in obj.ToArray())
                {
                    var child = path.Length == 0 ? property.Key : path + "/" + property.Key;
                    if (property.Value is JsonValue value
                        && value.TryGetValue<string>(out var text))
                    {
                        obj[property.Key] = Sanitize(child, text);
                    }
                    else
                    {
                        RedactStrings(property.Value, child);
                    }
                }

                break;
            case JsonArray array:
                var element = path + "[]";
                for (var index = 0; index < array.Count; index++)
                {
                    if (array[index] is JsonValue value
                        && value.TryGetValue<string>(out var text))
                    {
                        array[index] = Sanitize(element, text);
                    }
                    else
                    {
                        RedactStrings(array[index], element);
                    }
                }

                break;
        }
    }

    /// <summary>The whole decision for one string: withheld unless its position
    /// was reviewed, then withheld again unless the value is the shape that
    /// review assumed.</summary>
    /// <param name="path">Normalized position of the value.</param>
    /// <param name="value">The value as serialized.</param>
    /// <returns>The value itself, or the marker naming why it was dropped.</returns>
    private static string Sanitize(string path, string value)
    {
        if (!Allowlist.TryGetValue(path, out var shape))
        {
            return RedactedUnknownField;
        }

        var ok = shape switch
        {
            ValueShape.BuildVersion => IsTag(value, allowPlus: true),
            _ => IsTag(value, allowPlus: false),
        };
        return ok ? value : RedactedUnsafeValue;
    }

    /// <summary>fmf-core's <c>safe_diagnostic_tag</c>, restated: non-empty, at
    /// most <see cref="TagLengthCap"/> bytes, and nothing outside
    /// <c>[A-Za-z0-9._:-]</c> (plus <c>+</c> for a build version). A path, a
    /// file name or a query cannot survive it, which is what makes the
    /// allowlist a second line rather than the only one.</summary>
    /// <param name="value">The candidate value.</param>
    /// <param name="allowPlus">Whether <c>+</c> is part of the shape.</param>
    /// <returns>True when the value is a finite tag.</returns>
    private static bool IsTag(string value, bool allowPlus) =>
        value.Length is > 0 and <= TagLengthCap
        && value.All(ch =>
            char.IsAsciiLetterOrDigit(ch)
            || ch is '-' or '_' or '.' or ':'
            || (allowPlus && ch is '+'));

    private static readonly JsonSerializerOptions IndentedJson = new(EngineJson.SnakeCase)
    {
        WriteIndented = true,
    };
}
