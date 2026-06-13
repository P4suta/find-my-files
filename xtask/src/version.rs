//! Version-bump editors for the two files the release version lives in. Kept as
//! pure `&str -> Result<String>` transforms so the fiddly bits (preserving the
//! Cargo.toml comment, anchoring the csproj replace on exactly one marker) are
//! unit-tested without touching the filesystem.

pub mod cargo_toml;
pub mod csproj;
