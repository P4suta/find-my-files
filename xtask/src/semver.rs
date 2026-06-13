//! Release versions are plain `X.Y.Z` triples (no pre-release/build metadata).
//! Validating up front turns a typo into a clear error before any file is
//! rewritten or any tag is cut — the old PowerShell recipe had no such guard.

use anyhow::{bail, Result};

/// Accept only strict `X.Y.Z` where each part is one-or-more ASCII digits.
pub fn validate(version: &str) -> Result<()> {
    let parts: Vec<&str> = version.split('.').collect();
    let ok = parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
    if !ok {
        bail!("version must be X.Y.Z with digit-only parts, got '{version}'");
    }
    Ok(())
}

/// Strip a single leading `v`/`V` from a release tag (`v0.2.0` → `0.2.0`).
pub fn strip_tag_v(tag: &str) -> &str {
    tag.strip_prefix('v')
        .or_else(|| tag.strip_prefix('V'))
        .unwrap_or(tag)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_plain_triples() {
        for v in ["0.1.0", "1.2.3", "10.20.30", "0.0.0"] {
            assert!(validate(v).is_ok(), "{v} should be valid");
        }
    }

    #[test]
    fn rejects_malformed_versions() {
        for v in [
            "1.2",
            "1.2.3.4",
            "v1.2.3",
            "1.2.x",
            "1..3",
            "",
            "1.2.3-rc1",
            "1.2. 3",
        ] {
            assert!(validate(v).is_err(), "{v} should be rejected");
        }
    }

    #[test]
    fn strips_a_single_leading_v() {
        assert_eq!(strip_tag_v("v0.2.0"), "0.2.0");
        assert_eq!(strip_tag_v("V0.2.0"), "0.2.0");
        assert_eq!(strip_tag_v("0.2.0"), "0.2.0");
        // Only one leading v is stripped.
        assert_eq!(strip_tag_v("vv1.0.0"), "v1.0.0");
    }
}
