//! Bump `[workspace.package] version` in engine/Cargo.toml.
//!
//! Uses toml_edit and reaches the value by key path, so it no longer depends on
//! the `# release version` line comment the way the old regex did — and it
//! preserves that comment (and all surrounding formatting) by reattaching the
//! original value decor.

use anyhow::{Context, Result};
use toml_edit::{DocumentMut, Value};

/// Return `toml_src` with `[workspace.package] version` set to `new_version`,
/// preserving layout, the trailing `# release version` comment, and line
/// endings.
pub fn set_version(toml_src: &str, new_version: &str) -> Result<String> {
    let mut doc: DocumentMut = toml_src
        .parse()
        .context("parse engine/Cargo.toml as TOML")?;

    let item = doc
        .get_mut("workspace")
        .and_then(|w| w.get_mut("package"))
        .and_then(|p| p.get_mut("version"))
        .context("engine/Cargo.toml has no [workspace.package] version")?;
    let val = item
        .as_value_mut()
        .context("[workspace.package] version is not a plain value")?;

    // Replace the string but keep the existing decor (whitespace + the trailing
    // `# release version` comment live in the value's suffix decor).
    let decor = val.decor().clone();
    let mut new_val = Value::from(new_version);
    *new_val.decor_mut() = decor;
    *val = new_val;

    Ok(doc.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
[workspace.package]
version = \"0.1.0\" # release version — bump via 'just release'
edition = \"2024\"
";

    #[test]
    fn bumps_the_version() {
        let out = set_version(SAMPLE, "0.2.0").unwrap();
        assert!(out.contains("version = \"0.2.0\""), "got:\n{out}");
        assert!(!out.contains("0.1.0"), "old version lingered:\n{out}");
    }

    #[test]
    fn preserves_the_release_comment_and_neighbours() {
        let out = set_version(SAMPLE, "0.2.0").unwrap();
        assert!(
            out.contains("version = \"0.2.0\" # release version — bump via 'just release'"),
            "comment not preserved:\n{out}"
        );
        assert!(out.contains("edition = \"2024\""));
    }

    #[test]
    fn preserves_lf_line_endings() {
        let out = set_version(SAMPLE, "0.2.0").unwrap();
        assert!(!out.contains('\r'), "CR snuck in:\n{out:?}");
    }

    #[test]
    fn errors_when_the_version_key_is_absent() {
        let err = set_version("[workspace.package]\nedition = \"2024\"\n", "0.2.0");
        assert!(err.is_err());
    }
}
