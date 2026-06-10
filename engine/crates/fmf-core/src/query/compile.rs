//! AST → compiled matcher lists. Terms inside each AND group are ordered by
//! evaluation cost (numeric filters → memmem → regex → path) so the cheap
//! filters short-circuit the expensive ones (Everything's "compiled byte
//! code" idea, docs/RESEARCH.md).

use memchr::memmem;
use regex::bytes::{Regex, RegexBuilder};
use thiserror::Error;

use super::ast::{Ast, Term};
use super::dates::DateResolver;
use crate::wtf8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaseMode {
    /// Case-insensitive unless the needle contains an uppercase letter.
    Smart,
    Insensitive,
    Sensitive,
}

#[derive(Debug, Error)]
pub enum CompileError {
    #[error("invalid regex `{pattern}`: {source}")]
    Regex {
        pattern: String,
        source: regex::Error,
    },
}

pub(super) enum Matcher {
    /// Empty needle — matches everything.
    True,
    /// Substring in the name. `folded` selects the lower pool + folded needle.
    NameSub {
        finder: memmem::Finder<'static>,
        folded: bool,
    },
    /// Substring in the full path.
    PathSub {
        finder: memmem::Finder<'static>,
        folded: bool,
    },
    /// Anchored wildcard or user regex over the (original) name bytes.
    NameRegex {
        re: Regex,
    },
    /// Unanchored wildcard/regex over the (original) full-path bytes.
    PathRegex {
        re: Regex,
    },
    /// Extension equals any of these folded byte strings.
    Ext {
        exts: Vec<Vec<u8>>,
    },
    Size {
        min: u64,
        max: u64,
    },
    /// Inclusive FILETIME tick range.
    Mtime {
        min: i64,
        max: i64,
    },
    IsDir(bool),
}

impl Matcher {
    fn cost(&self) -> u8 {
        match self {
            Matcher::True | Matcher::Size { .. } | Matcher::Mtime { .. } | Matcher::IsDir(_) => 0,
            Matcher::Ext { .. } => 1,
            Matcher::NameSub { .. } => 2,
            Matcher::NameRegex { .. } => 3,
            Matcher::PathSub { .. } => 4,
            Matcher::PathRegex { .. } => 5,
        }
    }

    fn needs_folded_path(&self) -> bool {
        matches!(self, Matcher::PathSub { folded: true, .. })
    }

    fn needs_orig_path(&self) -> bool {
        matches!(
            self,
            Matcher::PathSub { folded: false, .. } | Matcher::PathRegex { .. }
        )
    }
}

pub(super) struct CTerm {
    pub negated: bool,
    pub matcher: Matcher,
}

pub struct CompiledQuery {
    pub(super) groups: Vec<Vec<CTerm>>,
    pub(super) needs_folded_paths: bool,
    pub(super) needs_orig_paths: bool,
}

/// Smart-case decision for one needle.
fn insensitive(needle: &str, case: CaseMode) -> bool {
    match case {
        CaseMode::Insensitive => true,
        CaseMode::Sensitive => false,
        CaseMode::Smart => !wtf8::has_uppercase(needle),
    }
}

fn substring_finder(needle: &str, case: CaseMode) -> (memmem::Finder<'static>, bool) {
    if insensitive(needle, case) {
        let folded = wtf8::fold_str(needle);
        (memmem::Finder::new(folded.as_bytes()).into_owned(), true)
    } else {
        (memmem::Finder::new(needle.as_bytes()).into_owned(), false)
    }
}

/// Translate a `*`/`?` pattern into a regex body (no anchors).
fn wildcard_to_regex_body(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len() * 2);
    for c in pattern.chars() {
        match c {
            '*' => out.push_str(".*"),
            '?' => out.push('.'),
            c => out.push_str(&regex::escape(&c.to_string())),
        }
    }
    out
}

fn build_regex(body: &str, ci: bool, pattern_for_err: &str) -> Result<Regex, CompileError> {
    RegexBuilder::new(body)
        .case_insensitive(ci)
        .dot_matches_new_line(true)
        .build()
        .map_err(|source| CompileError::Regex {
            pattern: pattern_for_err.to_string(),
            source,
        })
}

fn compile_term(
    term: &Term,
    case: CaseMode,
    dates: &dyn DateResolver,
) -> Result<CTerm, CompileError> {
    let (negated, term) = match term {
        Term::Not(inner) => (true, inner.as_ref()),
        t => (false, t),
    };

    let matcher = match term {
        Term::Name(s) if s.is_empty() => Matcher::True,
        Term::Name(s) => {
            let (finder, folded) = substring_finder(s, case);
            Matcher::NameSub { finder, folded }
        }
        Term::Path(s) => {
            let (finder, folded) = substring_finder(s, case);
            Matcher::PathSub { finder, folded }
        }
        Term::Wildcard(s) => {
            let body = format!("^{}$", wildcard_to_regex_body(s));
            Matcher::NameRegex {
                re: build_regex(&body, insensitive(s, case), s)?,
            }
        }
        Term::PathWildcard(s) => Matcher::PathRegex {
            re: build_regex(&wildcard_to_regex_body(s), insensitive(s, case), s)?,
        },
        Term::Regex(s) => Matcher::NameRegex {
            re: build_regex(s, insensitive(s, case), s)?,
        },
        Term::Ext(exts) => Matcher::Ext {
            exts: exts
                .iter()
                .map(|e| wtf8::fold_str(e).into_bytes())
                .collect(),
        },
        Term::Size { min, max } => Matcher::Size {
            min: *min,
            max: *max,
        },
        // [start, end) at local midnight → inclusive tick range.
        Term::Mtime { start, end } => Matcher::Mtime {
            min: start.map_or(i64::MIN, |c| dates.filetime_at_midnight(c)),
            max: end.map_or(i64::MAX, |c| {
                dates.filetime_at_midnight(c).saturating_sub(1)
            }),
        },
        Term::IsDir(d) => Matcher::IsDir(*d),
        Term::Not(_) => unreachable!("nested Not is flattened by the parser"),
    };

    Ok(CTerm { negated, matcher })
}

pub fn compile(
    ast: &Ast,
    case: CaseMode,
    dates: &dyn DateResolver,
) -> Result<CompiledQuery, CompileError> {
    let mut groups = Vec::with_capacity(ast.groups.len());
    for g in &ast.groups {
        let mut terms = Vec::with_capacity(g.len());
        for t in g {
            terms.push(compile_term(t, case, dates)?);
        }
        terms.sort_by_key(|t| t.matcher.cost());
        groups.push(terms);
    }

    let needs_folded_paths = groups
        .iter()
        .flatten()
        .any(|t| t.matcher.needs_folded_path());
    let needs_orig_paths = groups.iter().flatten().any(|t| t.matcher.needs_orig_path());
    Ok(CompiledQuery {
        groups,
        needs_folded_paths,
        needs_orig_paths,
    })
}

impl CompiledQuery {
    pub(super) fn needs_paths(&self) -> bool {
        self.needs_folded_paths || self.needs_orig_paths
    }
}
