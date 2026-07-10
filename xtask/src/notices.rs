//! Assembly of the bundle's `THIRD-PARTY-NOTICES.txt`.
//!
//! The shipped zip redistributes third-party code under permissive licenses that
//! require their notices to travel with the binaries: Apache-2.0 §4 wants the
//! license text shipped, MIT wants the copyright + permission notice preserved.
//! Two families of dependency need covering:
//!
//!   1. The Rust crates statically linked into `fmf_engine.dll`, `fmf.exe`,
//!      `fmf-service.exe` and the launcher — enumerated mechanically by
//!      `cargo-about` (config: `engine/about.toml`, template: `engine/about.hbs`),
//!      whose rendered text is passed in here as `rust_notices`.
//!   2. The .NET runtime + `NuGet` components the `WinUI` app publishes — a
//!      small, slow-moving, curated set (below), because they are not in the
//!      Cargo graph cargo-about walks.
//!
//! This module owns only the *pure* assembly (header + curated .NET section +
//! the Rust block) so it can be unit-tested without running cargo-about; the
//! placement + BOM/CRLF encoding + the cargo-about invocation live in `publish`.

/// Curated notices for the redistributed .NET / Windows App SDK / `NuGet`
/// components. These are the *shipped* `PackageReferences` from
/// `app/FindMyFiles/FindMyFiles.csproj` — the analyzers (`PrivateAssets=all`) and
/// the build-time SDKs (`Microsoft.Windows.SDK.BuildTools*`) are not
/// redistributed, and `WebView2` is stripped from the bundle (see `prune`), so
/// none of those appear here. Versions are given as the pinned major line
/// (the csproj floats the patch) so this text does not churn on a point bump.
///
/// The Apache-2.0 full text ships as the bundle's `LICENSE.txt` (the project's
/// own license) and the MIT text appears verbatim in the Rust block below, so
/// this section reproduces each component's copyright + license identification
/// and points at the canonical full terms rather than duplicating them.
pub const DOTNET_SECTION: &str = "\
Microsoft .NET runtime and base class libraries
  Copyright (c) .NET Foundation and Contributors.
  License: MIT — https://github.com/dotnet/runtime/blob/main/LICENSE.TXT
  The self-contained bundle embeds the .NET runtime (CoreCLR) and the base
  class libraries.

Windows App SDK / WinUI  (Microsoft.WindowsAppSDK.WinUI 2.x)
  (c) Microsoft Corporation. All rights reserved.
  License: Microsoft Software License Terms (proprietary).
  The full terms ship inside the NuGet package as license.txt; see also
  https://github.com/microsoft/WindowsAppSDK and https://aka.ms/WinAppSDK.

CommunityToolkit.Mvvm 8.x
  (c) .NET Foundation and Contributors. All rights reserved.
  License: MIT — https://github.com/CommunityToolkit/dotnet
  (The MIT license text appears in full in the Rust section below.)

Serilog 4.x
  Copyright (c) 2013-2024 Serilog Contributors
  License: Apache-2.0 — https://serilog.net/
  (The Apache-2.0 license text is the bundle's LICENSE.txt.)

Serilog.Sinks.File 6.x
  Copyright (c) Serilog Contributors
  License: Apache-2.0 — https://github.com/serilog/serilog-sinks-file
  (The Apache-2.0 license text is the bundle's LICENSE.txt.)
";

/// Section rule reused for the header + the two part banners.
const RULE: &str =
    "================================================================================";

/// Assemble the full `THIRD-PARTY-NOTICES.txt` body (LF; the caller adds the BOM
/// and CRLF for Notepad). `rust_notices` is the text `cargo-about` rendered from
/// `engine/about.hbs`. Pure — no cargo-about, no filesystem — so the shape is
/// unit-tested here and only the tool invocation lives in `publish`.
#[must_use]
pub fn assemble(rust_notices: &str) -> String {
    let mut out = String::new();
    out.push_str(RULE);
    out.push('\n');
    out.push_str("FindMyFiles — Third-Party Notices\n");
    out.push_str(RULE);
    out.push('\n');
    out.push_str(
        "\nThis distribution includes third-party software listed below. \
         FindMyFiles\nitself is licensed under Apache-2.0 — its full text is in \
         LICENSE.txt beside\nthis file. Each component below is redistributed \
         under its own terms.\n\n\n",
    );

    out.push_str(RULE);
    out.push('\n');
    out.push_str("PART 1 — .NET runtime, Windows App SDK and NuGet components\n");
    out.push_str(RULE);
    out.push_str("\n\n");
    out.push_str(DOTNET_SECTION);
    out.push_str("\n\n");

    out.push_str(RULE);
    out.push('\n');
    out.push_str(
        "PART 2 — Rust crates (statically linked into fmf_engine.dll, fmf.exe,\n\
         \x20        fmf-service.exe and the launcher)\n",
    );
    out.push_str(RULE);
    out.push('\n');
    // The cargo-about render already begins with its own section banner per
    // license; keep exactly one blank line before it.
    out.push('\n');
    out.push_str(rust_notices.trim_start_matches('\n'));
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotnet_section_names_every_shipped_component() {
        // The four shipped PackageReferences + the embedded runtime. A dropped
        // line here would ship a notice missing a redistributed component.
        for needle in [
            "Microsoft .NET runtime",
            "Windows App SDK / WinUI",
            "CommunityToolkit.Mvvm",
            "Serilog 4.x",
            "Serilog.Sinks.File 6.x",
        ] {
            assert!(
                DOTNET_SECTION.contains(needle),
                "curated .NET section must mention {needle}"
            );
        }
    }

    #[test]
    fn dotnet_section_omits_non_shipped_dependencies() {
        // Analyzers (PrivateAssets), build-time SDKs and the pruned WebView2
        // payload are not redistributed — they must not appear in the notice.
        for absent in [
            "Roslynator",
            "StyleCop",
            "Meziantou",
            "SDK.BuildTools",
            "WebView2",
        ] {
            assert!(
                !DOTNET_SECTION.contains(absent),
                "{absent} is not redistributed and must not appear in the notice"
            );
        }
    }

    #[test]
    fn assemble_embeds_both_parts_in_order() {
        let body = assemble("RUST_LICENSE_BLOCK_MARKER\n");
        let part1 = body.find("PART 1").expect("part 1 present");
        let part2 = body.find("PART 2").expect("part 2 present");
        assert!(part1 < part2, "PART 1 must precede PART 2");
        // The curated .NET text lands in part 1, the cargo-about render in part 2.
        let dotnet = body.find("Microsoft .NET runtime").expect("dotnet present");
        let rust = body
            .find("RUST_LICENSE_BLOCK_MARKER")
            .expect("rust present");
        assert!(
            part1 < dotnet && dotnet < part2,
            ".NET section is in PART 1"
        );
        assert!(part2 < rust, "the Rust block follows PART 2");
    }

    #[test]
    fn assemble_states_the_project_license_and_cross_reference() {
        let body = assemble("");
        assert!(
            body.contains("Apache-2.0") && body.contains("LICENSE.txt"),
            "the header must point at the project's own Apache-2.0 LICENSE.txt"
        );
        assert!(
            body.starts_with(RULE),
            "the file opens with the section rule, not a stray blank line"
        );
    }

    #[test]
    fn assemble_ends_with_a_single_newline() {
        let body = assemble("some crate notice");
        assert!(body.ends_with('\n'));
        assert!(!body.ends_with("\n\n"), "no trailing blank-line pileup");
    }
}
