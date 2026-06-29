using System.Reflection;

namespace FindMyFiles.Services;

/// <summary>
/// Channel-aware build identity (ADR-0035), surfaced in the launch log and the
/// F12 diagnostics panel. The string is stamped at build time from the csproj
/// <c>InformationalVersion</c> (the channel pre-release tag plus the Source Link
/// commit sha), mirroring the Rust <c>fmf_buildstamp::VERSION</c>: a contributor's
/// local build (<c>…-dev+&lt;sha&gt;</c>) is distinguishable from a nightly
/// (<c>…-nightly.&lt;date&gt;+&lt;sha&gt;</c>) or a clean stable release.
/// </summary>
public static class BuildInfo
{
    /// <summary>
    /// The channel-aware build version, e.g. <c>0.1.0-dev+&lt;sha&gt;</c>,
    /// <c>0.1.0-nightly.20260629+&lt;sha&gt;</c>, or <c>0.1.0+&lt;sha&gt;</c> for a stable release.
    /// Falls back to the assembly version, then <c>"unknown"</c>, if the attribute is absent.
    /// </summary>
    public static string Version { get; } =
        typeof(BuildInfo).Assembly
            .GetCustomAttribute<AssemblyInformationalVersionAttribute>()?
            .InformationalVersion
        ?? typeof(BuildInfo).Assembly.GetName().Version?.ToString()
        ?? "unknown";

    /// <summary>
    /// The bare <c>X.Y.Z</c> base of a build-version string — everything before
    /// the first <c>-</c> (pre-release) or <c>+</c> (build metadata). Used to
    /// compare the app against the engine without tripping on channel/sha.
    /// </summary>
    /// <param name="version">A build-version string (possibly empty).</param>
    /// <returns>The leading <c>X.Y.Z</c> base, or empty when <paramref name="version"/> is.</returns>
    public static string BaseOf(string version)
    {
        if (string.IsNullOrEmpty(version))
        {
            return string.Empty;
        }

        int cut = version.AsSpan().IndexOfAny('-', '+');
        return cut < 0 ? version : version[..cut];
    }

    /// <summary>True when two build-version strings share the same <c>X.Y.Z</c>
    /// base (e.g. app and engine are from the same release).</summary>
    /// <param name="a">First build-version string.</param>
    /// <param name="b">Second build-version string.</param>
    /// <returns>Whether the two share the same <c>X.Y.Z</c> base.</returns>
    public static bool SameBase(string a, string b) =>
        string.Equals(BaseOf(a), BaseOf(b), StringComparison.Ordinal);
}
