//! The per-invocation output context: the cross-cutting flags that shape how a
//! command presents itself, threaded from `main` into the commands that need
//! them. Colour is resolved into anstream's *global* choice (so the styled
//! `anstream::println!`/`eprintln!` macros pick it up everywhere); this struct
//! carries the rest.

use clap::ValueEnum;

/// How a command renders its result.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum Format {
    /// Human-readable text (tables, colour, the progress spinner).
    #[default]
    Human,
    /// A single machine-readable JSON document on stdout (NDJSON — one object
    /// per line — for streaming commands like `watch`). Not every command has a
    /// JSON form; those keep printing text.
    Json,
}

/// Cross-cutting presentation flags for one CLI invocation.
#[derive(Clone, Copy, Debug)]
pub struct Ctx {
    /// Suppress the progress spinner and other stderr chrome (`--quiet`).
    pub quiet: bool,
    /// The requested output format (`--format`).
    pub format: Format,
}

impl Ctx {
    /// Whether human chrome (spinner, progress lines, decorative stderr) should
    /// be shown: only in the default text format and when not quietened.
    #[must_use]
    pub const fn human_chrome(self) -> bool {
        !self.quiet && matches!(self.format, Format::Human)
    }

    /// Whether the command should emit machine-readable JSON.
    #[must_use]
    pub const fn is_json(self) -> bool {
        matches!(self.format, Format::Json)
    }
}
