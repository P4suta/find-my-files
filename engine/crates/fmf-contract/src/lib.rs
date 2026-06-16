//! fmf-contract — the machine-readable single source of the engine contract
//! (ADR-0018). The prose canon is docs/ARCHITECTURE.md; this crate is its
//! executable form, and every consumer radiates from here:
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
//! Section map (ARCHITECTURE.md → here):
//! - Error code table                  → [`codes`]
//! - Pipe opcode table                 → [`opcodes`]
//! - Events (FFI kind 1..=6)           → [`events`]
//! - `FmfQueryOptions` enum values     → [`options`]
//! - POD layout (`FmfRow` etc.)        → [`pod`]
//! - Volume label 16B packing          → [`volume`]
//! - ABI/protocol versions, pipe name  → [`versions`]
//! - Limits (16MiB, 64 entries etc.)   → [`limits`]

pub mod codes;
pub mod counters;
pub mod events;
pub mod limits;
pub mod opcodes;
pub mod options;
pub mod pod;
pub mod versions;
pub mod volume;
