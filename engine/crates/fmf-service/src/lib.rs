//! fmf-service — the privileged engine host: a named-pipe server over
//! fmf-core, speaking the fmf-proto wire (canonical spec:
//! docs/ARCHITECTURE.md "Pipe protocol"). Library form so the loopback
//! integration tests drive the same server the binary runs.

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
