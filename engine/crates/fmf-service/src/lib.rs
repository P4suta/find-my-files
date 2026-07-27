//! fmf-service — the privileged engine host: a named-pipe server over
//! fmf-core, speaking the fmf-proto wire.
//!
//! It exists so the everyday UI can stay unelevated: reading the $MFT and the
//! USN journal needs administrator rights, so exactly one `LocalSystem`
//! process holds them and hands filtered results across the pipe (ADR-0016).
//! Like `fmf-ffi`, this crate is a boundary: dispatch is a mapping from opcode
//! to an `Engine` call, and any logic belongs in fmf-core.
//!
//! Library form so the loopback integration tests drive the same server the
//! binary runs — the transport is covered unelevated, and only the real
//! volumes need `FMF_ADMIN_TESTS`.

pub mod config;
mod dispatch;
mod events;
mod faults;
mod host;
pub mod lifecycle;
pub mod pipe;
pub mod security;
pub mod server;
pub mod svc;
