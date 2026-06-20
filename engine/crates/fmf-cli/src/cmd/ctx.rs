//! The per-invocation output context: the cross-cutting flags that shape how a
//! command presents itself, threaded from `main` into the commands that need
//! them. Colour is resolved into anstream's *global* choice (so the styled
//! `anstream::println!`/`eprintln!` macros pick it up everywhere); this struct
//! carries the rest.

/// Cross-cutting presentation flags for one CLI invocation.
#[derive(Clone, Copy, Debug)]
pub struct Ctx {
    /// Suppress the progress spinner and other stderr chrome (`--quiet`).
    pub quiet: bool,
}
