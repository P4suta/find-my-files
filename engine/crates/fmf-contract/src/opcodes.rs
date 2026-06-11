//! Pipe opcodes (docs/ARCHITECTURE.md オペコード表). Event pushes reuse
//! 1..=6 as the event *kind* with `flags = event` — dispatch must branch on
//! the flag before the opcode.

pub const HELLO: u16 = 1;
pub const SUBSCRIBE: u16 = 2;
pub const UNSUBSCRIBE: u16 = 3;
pub const LIST_VOLUMES: u16 = 4;
pub const INDEX_START: u16 = 5;
pub const INDEX_STATUS: u16 = 6;
pub const QUERY: u16 = 7;
pub const RESULT_PAGE: u16 = 8;
pub const RESULT_FREE: u16 = 9;
pub const STATS: u16 = 10;
/// Number reserved, deliberately unimplemented (client-driven flush is a
/// local-DoS lever — ADR-0016).
pub const FLUSH_RESERVED: u16 = 11;
pub const SERVICE_INFO: u16 = 12;
