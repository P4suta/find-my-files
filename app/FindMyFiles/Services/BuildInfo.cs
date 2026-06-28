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
}
