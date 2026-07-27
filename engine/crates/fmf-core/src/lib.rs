//! fmf-core — the entire engine logic of find-my-files.
//!
//! This crate is a pure library: the boundary crates (`fmf-ffi`,
//! `fmf-service`) and the dev CLI (`fmf-cli`) must not contain logic of their
//! own — they convert, marshal and catch panics. The values those boundaries
//! agree on with the C# host live in `fmf-contract` (ADR-0018), and this
//! crate uses those enums directly rather than mapping to private twins.

// Declared in dataflow order — reading order = the order data moves
// (ingest: ondisk/mft/scan → usn → index; search: query → engine;
// cross-cutting last). Names are unchanged; only the narrative order is
// meaningful.
//
// The `#[cfg(windows)]` split is drawn at *acquisition*, not at subject
// matter: a module is gated only when it opens a handle or issues an FSCTL
// (mft / scan / engine / volume_label / usn::session, all windows-sys). The
// byte grammar those modules feed lives in `ondisk`, which is pure and
// therefore ungated — that is what lets engine/fuzz reach the NTFS decoders
// under libFuzzer on Linux (ADR-0047), alongside the other pure parsers
// (query / index::snapshot / usn::records / wtf8). No pure module references a
// gated one. The only Windows piece outside the gated modules —
// `query::dates::WindowsLocalResolver` — is already `#[cfg(windows)]` inside
// the (otherwise pure) query module.
pub mod ondisk;

#[cfg(windows)]
pub mod mft;
#[cfg(windows)]
pub mod scan;
pub mod usn;
#[cfg(windows)]
mod volume_label;

pub mod index;

pub mod query;

#[cfg(windows)]
pub mod engine;

pub mod diag;
pub mod metrics;
pub mod wtf8;
