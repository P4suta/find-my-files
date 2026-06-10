//! Query engine: text → AST → compiled matchers → parallel scan →
//! materialized, sort-ordered result (docs/ARCHITECTURE.md).
//!
//! Syntax (Everything-compatible core):
//! `space`=AND, `|`=OR (weakest), `!`=NOT, `"..."`=phrase, `*`/`?` wildcards
//! (match the whole name), a `\` inside a term switches it to path matching,
//! and the filters `ext:`, `path:`, `size:`, `dm:`, `regex:`, `file:`,
//! `folder:`.

mod ast;
mod compile;
mod dates;
mod exec;

pub use ast::{Ast, ParseError, Term, parse};
pub use compile::{CaseMode, CompileError, CompiledQuery, compile};
#[cfg(windows)]
pub use dates::WindowsLocalResolver;
pub use dates::{DateResolver, UtcResolver};
pub use exec::{SearchResult, search};

use crate::index::SortKey;

#[derive(Clone, Copy, Debug)]
pub struct QueryOptions {
    pub sort: SortKey,
    pub desc: bool,
    pub case: CaseMode,
    /// Hidden/system entries (and everything under such branches) are
    /// skipped unless this is set — the UI toggle maps straight here.
    pub include_hidden_system: bool,
}

impl Default for QueryOptions {
    fn default() -> Self {
        Self {
            sort: SortKey::Name,
            desc: false,
            case: CaseMode::Smart,
            include_hidden_system: false,
        }
    }
}
