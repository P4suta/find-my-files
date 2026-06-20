//! Machine-readable output: a stable-ish JSON envelope for `--format json`.
//!
//! Every payload carries a `format_version` so a script can detect a shape
//! change. The number is bumped only when an existing field changes meaning or
//! is removed — additive fields do not bump it.

use std::error::Error;

use serde::Serialize;

/// The JSON shape version stamped into every `--format json` payload.
pub const FORMAT_VERSION: u32 = 1;

/// `value` as a JSON object with a `format_version` field merged in. `value`
/// must serialise to a JSON object (the only shape the CLI emits at top level).
fn with_version<T: Serialize>(value: &T) -> Result<serde_json::Value, Box<dyn Error>> {
    let mut v = serde_json::to_value(value)?;
    if let serde_json::Value::Object(map) = &mut v {
        map.insert("format_version".to_owned(), FORMAT_VERSION.into());
    }
    Ok(v)
}

/// Print `value` as a pretty JSON document on stdout (`--format json`).
pub fn emit<T: Serialize>(value: &T) -> Result<(), Box<dyn Error>> {
    println!("{}", serde_json::to_string_pretty(&with_version(value)?)?);
    Ok(())
}

/// Print `value` as one compact JSON line on stdout (one NDJSON record, for
/// streaming commands like `watch`).
pub fn emit_line<T: Serialize>(value: &T) -> Result<(), Box<dyn Error>> {
    println!("{}", serde_json::to_string(&with_version(value)?)?);
    Ok(())
}
