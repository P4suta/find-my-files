//! Compile-time build identity for the front-end binaries (`fmf`, `fmf-service`).
//!
//! The string is resolved in `build.rs` (channel + git sha) and injected through
//! `rustc-env`; consumers read [`VERSION`] instead of `CARGO_PKG_VERSION` so a
//! contributor's local build (`…-dev+g<sha>`) is distinguishable from a nightly
//! (`…-nightly.<date>+g<sha>`) or a clean stable release (`X.Y.Z`).
//!
//! No logic lives here, and this crate is a leaf depended on ONLY by the binaries
//! — never by `fmf-core` / `fmf-ffi` (keeps the no-logic FFI boundary intact and
//! keeps the build.rs git probe from rebuilding the hot engine crates).

/// The channel-aware build version, e.g. `0.1.0-dev+g3672e3f`,
/// `0.1.0-nightly.20260629+g3672e3f`, or a clean `0.1.0` for a stable release.
pub const VERSION: &str = env!("FMF_VERSION_STRING");
