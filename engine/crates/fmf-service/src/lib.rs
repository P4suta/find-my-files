//! fmf-service — the privileged engine host: a named-pipe server over
//! fmf-core, speaking the fmf-proto wire (canonical spec:
//! docs/ARCHITECTURE.md「Pipe プロトコル」). Library form so the loopback
//! integration tests drive the same server the binary runs.

pub mod config;
pub mod dispatch;
pub mod events;
pub mod faults;
pub mod host;
pub mod pipe;
pub mod security;
pub mod server;
pub mod svc;
