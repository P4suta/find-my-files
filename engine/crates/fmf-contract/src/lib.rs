//! fmf-contract — the single source of the engine contract (ADR-0018).
//!
//! Every value both sides of a boundary must agree on lives here, and every
//! consumer radiates from this crate:
//!
//! ```text
//! app(C#: Engine/Generated/EngineContract.g.cs ← gen-contract)
//!   → IEngineClient → (fmf-ffi | fmf-service → fmf-proto) → fmf-core → fmf-contract
//! ```
//!
//! Allowed contents — constants, `#[repr]` types, layout assertions, and
//! pure byte conversions. **No logic** (no I/O, no engine types, no serde):
//! that hard line is what keeps `[dependencies]` empty, and the empty
//! dependency list is what lets the cdylib and every rlib share one
//! definition instead of pinned copies.
//!
//! Section map:
//! - Error code table                  → [`codes`]
//! - Degradation counter roster        → [`counters`]
//! - Pipe opcode table                 → [`opcodes`]
//! - Events (FFI kind 1..=6)           → [`events`]
//! - `FmfQueryOptions` enum values     → [`options`]
//! - POD layout (`FmfRow` etc.)        → [`pod`]
//! - Volume label 16B packing          → [`volume`]
//! - ABI/protocol versions, pipe name  → [`versions`]
//! - Allocation and payload bounds     → [`limits`]
//!
//! Two boundaries speak this contract with different transports: the
//! in-process C ABI (`fmf-ffi`, versioned by [`versions::ABI_VERSION`]) and
//! the named pipe (`fmf-proto` + `fmf-service`, versioned by
//! [`versions::PROTOCOL_VERSION`]). They share the status codes, event kinds,
//! enum values and row/options POD; where they legitimately differ, the
//! difference is documented on the item itself.

pub mod codes;
pub mod counters;
pub mod events;
pub mod limits;
pub mod opcodes;
pub mod options;
pub mod pod;
pub mod versions;
pub mod volume;
