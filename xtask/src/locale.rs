//! Locale-folder pruning predicate for the distributable bundle.
//!
//! WinAppSDK self-contained publish drops ~85 locale resource dirs; the bundle
//! keeps only the languages the app actually ships (lookups fall back to the
//! neutral resources when a locale dir is absent). This is the pure decision
//! the publish step applies to each top-level dir under dist/FindMyFiles.

use regex::Regex;
use std::sync::OnceLock;

/// Locale dirs to keep. Compared case-insensitively, matching the original
/// PowerShell `-notin` — and note the folder casing is itself inconsistent
/// (`en-us` vs `ja-JP`), which is exactly why ASCII-case-insensitive is the
/// faithful reproduction rather than an exact-string match.
const KEEP_LOCALES: &[&str] = &["en-us", "ja-JP", "zh-Hans", "zh-CN"];

/// Should this top-level dist/ subdirectory be pruned as an unwanted locale?
///
/// Reproduces the justfile predicate exactly: a name shaped like a BCP-47
/// locale folder — `^[a-z]{2,3}(-[A-Za-z0-9]+){1,3}$`, matched
/// case-insensitively like PowerShell `-match` — that is NOT in the keep-list.
/// Non-locale dirs (Microsoft.UI.Xaml, Assets, golden, Views, …) don't fit the
/// shape (dots aren't hyphens; a bare word has no hyphen segment) so they stay.
pub fn should_prune_locale_dir(name: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"(?i)^[a-z]{2,3}(-[A-Za-z0-9]+){1,3}$").expect("static locale regex")
    });
    re.is_match(name) && !KEEP_LOCALES.iter().any(|k| k.eq_ignore_ascii_case(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_shipped_locales() {
        for keep in ["en-us", "ja-JP", "zh-Hans", "zh-CN"] {
            assert!(!should_prune_locale_dir(keep), "{keep} must be kept");
        }
    }

    #[test]
    fn keeps_locales_regardless_of_case() {
        // Folder casing varies; the keep-list comparison is case-insensitive.
        for keep in ["en-US", "EN-US", "ja-jp", "ZH-HANS"] {
            assert!(!should_prune_locale_dir(keep), "{keep} must be kept");
        }
    }

    #[test]
    fn prunes_unshipped_locales() {
        for prune in ["fr-FR", "de-DE", "es-ES", "pt-BR", "zh-Hant", "af-za"] {
            assert!(should_prune_locale_dir(prune), "{prune} must be pruned");
        }
    }

    #[test]
    fn keeps_non_locale_dirs() {
        // The real payload dirs and files that must never be pruned.
        for keep in [
            "Microsoft.UI.Xaml",
            "Assets",
            "golden",
            "Views",
            "runtimes",
            "fmf_engine.dll",
            "WinRT.Runtime.dll",
        ] {
            assert!(!should_prune_locale_dir(keep), "{keep} must be kept");
        }
    }

    #[test]
    fn a_bare_language_with_no_region_is_not_a_match() {
        // The shape requires at least one hyphen segment, so `en`/`qps` stay.
        assert!(!should_prune_locale_dir("en"));
        assert!(!should_prune_locale_dir("qps"));
    }
}
