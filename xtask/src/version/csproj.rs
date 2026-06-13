//! Bump `<Version>` in FindMyFiles.csproj.
//!
//! XML, so `toml_edit` can't help; we anchor a regex on the same `<!-- release
//! version` marker comment the file already carries and require EXACTLY ONE
//! match. That single-match guard is the whole point — it turns a silent no-op
//! (marker moved/renamed) or a multi-replace into a hard error, the failure
//! mode the old `-replace` could hit silently. `PackageReference` `Version="…"`
//! attributes use a different syntax and never match.

use anyhow::{bail, Result};
use regex::Regex;
use std::sync::OnceLock;

fn marker_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // Capture the surrounding tags so only the digits are swapped; the
        // trailing whitespace before the marker comment is part of `post` and
        // re-emitted verbatim.
        Regex::new(r"(?P<pre><Version>)\d+\.\d+\.\d+(?P<post></Version>\s*<!-- release version)")
            .expect("static csproj marker regex")
    })
}

/// Return `csproj_src` with the marked `<Version>` set to `new_version`.
/// Errors unless the marker matches exactly once.
pub fn set_version(csproj_src: &str, new_version: &str) -> Result<String> {
    let re = marker_re();
    let count = re.find_iter(csproj_src).count();
    if count != 1 {
        bail!(
            "expected exactly 1 `<Version>… <!-- release version` marker in csproj, found {count}"
        );
    }
    let out = re.replace(csproj_src, format!("${{pre}}{new_version}${{post}}"));
    Ok(out.into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
  <PropertyGroup>
    <OutputType>WinExe</OutputType>
    <Version>0.1.0</Version> <!-- release version — bump via 'just release' -->
    <TargetFramework>net10.0-windows10.0.26100.0</TargetFramework>
  </PropertyGroup>
  <ItemGroup>
    <PackageReference Include=\"CommunityToolkit.Mvvm\" Version=\"8.4.2\" />
  </ItemGroup>
";

    #[test]
    fn bumps_only_the_marked_version() {
        let out = set_version(SAMPLE, "0.2.0").unwrap();
        assert!(
            out.contains("<Version>0.2.0</Version> <!-- release version"),
            "got:\n{out}"
        );
        // The PackageReference version is untouched.
        assert!(out.contains("Version=\"8.4.2\""));
        assert!(!out.contains("<Version>0.1.0</Version>"));
    }

    #[test]
    fn errors_when_the_marker_is_missing() {
        let no_marker = "<Project><Version>0.1.0</Version></Project>";
        assert!(set_version(no_marker, "0.2.0").is_err());
    }

    #[test]
    fn errors_when_the_marker_appears_twice() {
        let doubled = format!("{SAMPLE}{SAMPLE}");
        let err = set_version(&doubled, "0.2.0");
        assert!(err.is_err(), "two markers must be rejected");
    }
}
