//! Query engine: text → AST → compiled matchers → parallel scan →
//! materialized, sort-ordered result (docs/ARCHITECTURE.md).
//!
//! Syntax (core):
//! `space`=AND, `|`=OR (weakest), `!`=NOT, `"..."`=phrase, `*`/`?` wildcards
//! (match the whole name), a `\` inside a term switches it to path matching,
//! and the filters `ext:`, `path:`, `size:`, `dm:`, `regex:`, `file:`,
//! `folder:`.

mod ast;
mod compile;
mod dates;
mod exec;
mod matchers;
mod memo;
mod subsume;
mod sweep;

pub use ast::{Ast, ParseError, Term, parse};
pub use compile::{CaseMode, CompileError, CompiledQuery, compile, compile_whole_regex};
#[cfg(windows)]
pub use dates::WindowsLocalResolver;
pub use dates::{DateResolver, UtcResolver};
pub(crate) use exec::refine;
pub use exec::{SearchMetrics, SearchResult, search};
pub use fmf_contract::options::RegexScope;
pub use memo::{derived_cache_bytes, prewarm};
pub(crate) use subsume::subsumes;

use crate::index::SortKey;

/// Per-query options controlling sort order, case handling, visibility, and
/// whole-query regex mode — the engine-side form the wire options convert into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QueryOptions {
    /// Which column the materialized result is sorted by (name, size, date…).
    pub sort: SortKey,
    /// Sort descending when set, ascending otherwise.
    pub desc: bool,
    /// Case-sensitivity policy applied to matchers (smart/sensitive/insensitive).
    pub case: CaseMode,
    /// Hidden/system entries (and everything under such branches) are
    /// skipped unless this is set — the UI toggle maps straight here.
    pub include_hidden_system: bool,
    /// Treat the whole query text as one regex (`regex_mode` bit0) — the
    /// engine skips parsing and compiles a single `regex_scope` matcher.
    pub regex_mode: bool,
    /// Which haystack the whole-query regex runs against (ignored unless
    /// `regex_mode`).
    pub regex_scope: RegexScope,
}

impl Default for QueryOptions {
    fn default() -> Self {
        Self {
            sort: SortKey::Name,
            desc: false,
            case: CaseMode::Smart,
            include_hidden_system: false,
            regex_mode: false,
            regex_scope: RegexScope::Name,
        }
    }
}

/// The single wire→engine options conversion — both boundaries (FFI
/// `fmf_query` and pipe dispatch) go through this (ADR-0018). `regex_mode`
/// is a packed u32: bit0 = whole-query regex on, bit1 = scope (0 name /
/// 1 path).
impl From<fmf_contract::pod::FmfQueryOptions> for QueryOptions {
    fn from(o: fmf_contract::pod::FmfQueryOptions) -> Self {
        Self {
            sort: SortKey::from_u32(o.sort),
            desc: o.desc != 0,
            case: CaseMode::from_u32(o.case_mode),
            include_hidden_system: o.include_hidden_system != 0,
            regex_mode: o.regex_mode & 0b1 != 0,
            regex_scope: RegexScope::from_u32((o.regex_mode >> 1) & 0b1),
        }
    }
}
