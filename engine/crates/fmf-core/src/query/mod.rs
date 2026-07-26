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
pub(crate) mod dates;
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
pub use exec::{SearchMetrics, SearchResult, search};
pub(crate) use exec::{refine_cancellable, search_cancellable};
pub use fmf_contract::options::RegexScope;
pub use memo::{derived_cache_bytes, prewarm};
pub(crate) use subsume::subsumes;

use crate::index::SortKey;

/// A wire query option used an unknown enum, a non-Boolean value, or a
/// reserved bit. Boundaries reject this instead of silently changing the
/// caller's request.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("invalid FmfQueryOptions field {field}: {value}")]
pub struct QueryOptionsError {
    field: &'static str,
    value: u32,
}

impl QueryOptionsError {
    const fn new(field: &'static str, value: u32) -> Self {
        Self { field, value }
    }
}

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
impl TryFrom<fmf_contract::pod::FmfQueryOptions> for QueryOptions {
    type Error = QueryOptionsError;

    fn try_from(o: fmf_contract::pod::FmfQueryOptions) -> Result<Self, Self::Error> {
        let sort = match o.sort {
            0 => SortKey::Name,
            1 => SortKey::Size,
            2 => SortKey::Mtime,
            value => return Err(QueryOptionsError::new("sort", value)),
        };
        let desc = match o.desc {
            0 => false,
            1 => true,
            value => return Err(QueryOptionsError::new("desc", value)),
        };
        let case = match o.case_mode {
            0 => CaseMode::Smart,
            1 => CaseMode::Insensitive,
            2 => CaseMode::Sensitive,
            value => return Err(QueryOptionsError::new("case_mode", value)),
        };
        let include_hidden_system = match o.include_hidden_system {
            0 => false,
            1 => true,
            value => {
                return Err(QueryOptionsError::new("include_hidden_system", value));
            }
        };
        if o.regex_mode & !0b11 != 0 {
            return Err(QueryOptionsError::new("regex_mode", o.regex_mode));
        }
        if o._reserved != 0 {
            return Err(QueryOptionsError::new("_reserved", o._reserved));
        }
        Ok(Self {
            sort,
            desc,
            case,
            include_hidden_system,
            regex_mode: o.regex_mode & 0b1 != 0,
            regex_scope: if o.regex_mode & 0b10 == 0 {
                RegexScope::Name
            } else {
                RegexScope::Path
            },
        })
    }
}

#[cfg(test)]
mod option_tests {
    use super::*;
    use fmf_contract::pod::FmfQueryOptions;

    #[test]
    fn wire_options_reject_unknown_enums_non_booleans_and_reserved_bits() {
        for invalid in [
            FmfQueryOptions {
                sort: 3,
                ..FmfQueryOptions::default()
            },
            FmfQueryOptions {
                desc: 2,
                ..FmfQueryOptions::default()
            },
            FmfQueryOptions {
                case_mode: 3,
                ..FmfQueryOptions::default()
            },
            FmfQueryOptions {
                include_hidden_system: 2,
                ..FmfQueryOptions::default()
            },
            FmfQueryOptions {
                regex_mode: 4,
                ..FmfQueryOptions::default()
            },
            FmfQueryOptions {
                _reserved: 1,
                ..FmfQueryOptions::default()
            },
        ] {
            assert!(QueryOptions::try_from(invalid).is_err());
        }
    }

    #[test]
    fn wire_options_accept_every_documented_value() {
        for sort in 0..=2 {
            for desc in 0..=1 {
                for case_mode in 0..=2 {
                    for include_hidden_system in 0..=1 {
                        for regex_mode in 0..=3 {
                            let wire = FmfQueryOptions {
                                sort,
                                desc,
                                case_mode,
                                include_hidden_system,
                                regex_mode,
                                ..FmfQueryOptions::default()
                            };
                            assert!(QueryOptions::try_from(wire).is_ok(), "{wire:?}");
                        }
                    }
                }
            }
        }
    }
}
