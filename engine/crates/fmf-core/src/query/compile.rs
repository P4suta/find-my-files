//! AST → compiled execution plan. Each AND group gets a *driver* — the most
//! selective positive literal, executed as a single SIMD sweep over the name
//! pool — plus residual matchers ordered by evaluation cost (numeric filters
//! → memmem → regex → path).

use memchr::memmem;
use regex::bytes::{Regex, RegexBuilder};
use thiserror::Error;

use super::ast::{Ast, Term};
use super::dates::DateResolver;
use crate::wtf8;

// The case mode / regex scope are contract surface (FmfQueryOptions carries
// them as u32) — the canonical definitions are used directly (ADR-0018).
pub use fmf_contract::options::{CaseMode, RegexScope};

/// Why a query failed to compile into an executable plan.
#[derive(Debug, Error)]
pub enum CompileError {
    /// A `regex:`/`path:`-regex (or whole-query) pattern is invalid syntax or
    /// exceeds the compile size limit (`REGEX_SIZE_LIMIT`, ADR-0023).
    #[error("invalid regex `{pattern}`: {source}")]
    Regex {
        /// The offending pattern text, as written in the query.
        pattern: String,
        /// The underlying error from the `regex` crate's builder.
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
    /// Name starts with the bytes (`lit*`).
    NamePrefix {
        bytes: Vec<u8>,
        folded: bool,
    },
    /// Name ends with the bytes (`*.lit`).
    NameSuffix {
        bytes: Vec<u8>,
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
    const fn cost(&self) -> u8 {
        match self {
            Self::True | Self::Size { .. } | Self::Mtime { .. } | Self::IsDir(_) => 0,
            Self::Ext { .. } | Self::NamePrefix { .. } | Self::NameSuffix { .. } => 1,
            Self::NameSub { .. } => 2,
            Self::NameRegex { .. } => 3,
            Self::PathSub { .. } => 4,
            Self::PathRegex { .. } => 5,
        }
    }

    const fn needs_folded_path(&self) -> bool {
        matches!(self, Self::PathSub { folded: true, .. })
    }

    const fn needs_orig_path(&self) -> bool {
        matches!(
            self,
            Self::PathSub { folded: false, .. } | Self::PathRegex { .. }
        )
    }
}

pub(super) struct CTerm {
    pub negated: bool,
    pub matcher: Matcher,
    /// Derived for case-exact name literals: the needle is *not* its own
    /// fold (it contains an uppercase/foldable character). Such a needle
    /// can never occur in a fold-identical name — the matcher's O(1)
    /// reject (matchers.rs, ADR-0004).
    pub exact_needle_unstable: bool,
}

/// Candidate generator for one AND group — a single sweep over the folded
/// name pool (the only contiguous one) instead of a per-entry matcher call.
/// Needles are always folded; a case-exact source term makes the sweep a
/// superset and its exact comparison runs as a residual
/// (`CompiledGroup::driver_exact`).
// The Finder-carrying variants dwarf the unit ones; boxing would add an
// indirection to the hottest call in the engine for no measurable win.
#[allow(clippy::large_enum_variant)]
pub(super) enum Driver {
    /// No usable positive literal: evaluate every entry.
    FullScan,
    /// Group has no terms at all (empty query / bare `folder:`-less group).
    MatchAll,
    Sub {
        finder: memmem::Finder<'static>,
        needle_len: usize,
    },
    Prefix {
        bytes: Vec<u8>,
    },
    Suffixes {
        suffixes: Vec<Vec<u8>>,
        files_only: bool,
    },
}

impl Driver {
    pub(super) const fn label(&self) -> &'static str {
        match self {
            Self::FullScan => "full-scan",
            Self::MatchAll => "match-all",
            Self::Sub { .. } => "pool-scan",
            Self::Prefix { .. } => "prefix",
            Self::Suffixes { .. } => "suffix",
        }
    }
}

pub(super) struct CompiledGroup {
    pub driver: Driver,
    /// Residual matchers (cost-ordered); the driver's own condition is fully
    /// checked by the sweep and removed from here.
    pub terms: Vec<CTerm>,
    /// The term the driver was built from (None for FullScan/MatchAll).
    /// The sweep never reads it — it exists so cached-query refinement can
    /// re-evaluate the *complete* group per candidate (`exec::refine`), so
    /// subsumption sees every condition (subsume.rs), and so the exec can
    /// verify it per candidate when the sweep was a superset (below).
    pub driver_term: Option<CTerm>,
    /// False when the source term is case-exact: the folded sweep then
    /// over-approximates and `driver_term` must be verified per candidate.
    pub driver_exact: bool,
}

impl CompiledGroup {
    /// Every condition of this AND group: the driver's source term (most
    /// selective, so first) followed by the cost-ordered residuals.
    pub(super) fn all_terms(&self) -> impl Iterator<Item = &CTerm> {
        self.driver_term.iter().chain(self.terms.iter())
    }

    /// The conditions the sweep did *not* fully check: the residuals, plus
    /// the driver's source term when the sweep was a folded superset.
    pub(super) fn residual_terms(&self) -> impl Iterator<Item = &CTerm> {
        self.driver_term
            .iter()
            .filter(|_| !self.driver_exact)
            .chain(self.terms.iter())
    }
}

/// An executable plan: one compiled AND group per OR clause, plus the path
/// pools the sweep must materialize to evaluate them.
pub struct CompiledQuery {
    pub(super) groups: Vec<CompiledGroup>,
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

fn fold_needle(needle: &str, case: CaseMode) -> (Vec<u8>, bool) {
    if insensitive(needle, case) {
        (wtf8::fold_str(needle).into_bytes(), true)
    } else {
        (needle.as_bytes().to_vec(), false)
    }
}

fn substring_finder(needle: &str, case: CaseMode) -> (memmem::Finder<'static>, bool) {
    let (bytes, folded) = fold_needle(needle, case);
    (memmem::Finder::new(&bytes).into_owned(), folded)
}

/// `lit*` / `*lit` / `*lit*` style patterns collapse to anchored byte
/// comparisons; everything else stays a regex.
enum WildShape {
    Prefix(String),
    Suffix(String),
    Inner(String),
    General,
}

fn classify_wildcard(pattern: &str) -> WildShape {
    if pattern.contains('?') {
        return WildShape::General;
    }
    let starts = pattern.starts_with('*');
    let ends = pattern.ends_with('*');
    let inner = pattern.trim_matches('*');
    if inner.contains('*') || inner.is_empty() {
        return WildShape::General; // "a*b", "**", "*"
    }
    match (starts, ends) {
        (true, true) => WildShape::Inner(inner.to_string()),
        (true, false) => WildShape::Suffix(inner.to_string()),
        (false, true) => WildShape::Prefix(inner.to_string()),
        (false, false) => WildShape::General, // no '*' at all → parser bug
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

/// Compile-time bounds on a user regex (ADR-0023). The `regex` crate matches
/// in guaranteed linear time (finite automata, no backtracking) — so there is
/// no `ReDoS` *execution* blowup — but a pathological pattern can still demand a
/// large program/DFA at *build* time. We index file names only (p99 ≈110 B),
/// so a legitimate name regex never approaches 1 MiB; capping there turns a
/// memory-DoS pattern into a clean `CompiledTooBig` → `FMF_E_QUERY_SYNTAX`
/// rejection (it flows through `CompileError::Regex` unchanged), instead of
/// letting the elevated service compile it. Both default higher (10/2 MiB).
const REGEX_SIZE_LIMIT: usize = 1 << 20;
const REGEX_DFA_SIZE_LIMIT: usize = 1 << 20;

fn build_regex(body: &str, ci: bool, pattern_for_err: &str) -> Result<Regex, CompileError> {
    RegexBuilder::new(body)
        .case_insensitive(ci)
        .dot_matches_new_line(true)
        .size_limit(REGEX_SIZE_LIMIT)
        .dfa_size_limit(REGEX_DFA_SIZE_LIMIT)
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
        Term::Wildcard(s) => match classify_wildcard(s) {
            WildShape::Prefix(lit) => {
                let (bytes, folded) = fold_needle(&lit, case);
                Matcher::NamePrefix { bytes, folded }
            }
            WildShape::Suffix(lit) => {
                let (bytes, folded) = fold_needle(&lit, case);
                Matcher::NameSuffix { bytes, folded }
            }
            WildShape::Inner(lit) => {
                let (finder, folded) = substring_finder(&lit, case);
                Matcher::NameSub { finder, folded }
            }
            WildShape::General => {
                let body = format!("^{}$", wildcard_to_regex_body(s));
                Matcher::NameRegex {
                    re: build_regex(&body, insensitive(s, case), s)?,
                }
            }
        },
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

    let unstable = |bytes: &[u8]| {
        let s = std::str::from_utf8(bytes).expect("query needles are valid UTF-8");
        wtf8::has_uppercase(s)
    };
    let exact_needle_unstable = match &matcher {
        Matcher::NameSub {
            finder,
            folded: false,
        } => unstable(finder.needle()),
        Matcher::NamePrefix {
            bytes,
            folded: false,
        }
        | Matcher::NameSuffix {
            bytes,
            folded: false,
        } => unstable(bytes),
        _ => false,
    };
    Ok(CTerm {
        negated,
        matcher,
        exact_needle_unstable,
    })
}

/// Driver candidate score — longer literals are more selective. Returns
/// None for matchers that cannot drive a pool sweep.
fn driver_score(t: &CTerm) -> Option<usize> {
    if t.negated {
        return None;
    }
    match &t.matcher {
        Matcher::NameSub { finder, .. } => Some(finder.needle().len() * 2),
        Matcher::NamePrefix { bytes, .. } | Matcher::NameSuffix { bytes, .. } => {
            Some(bytes.len() * 2)
        }
        // The sweep needle is ".<ext>" — score like the other literals.
        Matcher::Ext { exts } if !exts.is_empty() => {
            Some(exts.iter().map(|e| (e.len() + 1) * 2).min().unwrap_or(0))
        }
        _ => None,
    }
}

/// Fold a case-exact needle for the superset sweep. Needles always
/// originate from the query `&str`, so the bytes are valid UTF-8; the
/// fold's length preservation keeps prefix/suffix anchors sound.
fn fold_exact_needle(bytes: &[u8]) -> Vec<u8> {
    let s = std::str::from_utf8(bytes).expect("query needles are valid UTF-8");
    wtf8::fold_str(s).into_bytes()
}

/// Build the sweep driver from a term, leaving the term intact (kept as
/// `CompiledGroup::driver_term`). Returns the driver and whether it fully
/// checks the term — false for a case-exact term: the sweep folds its
/// needle (sound: an original-case match always implies the folded match)
/// and the exact comparison runs as a residual.
fn driver_for(t: &CTerm) -> (Driver, bool) {
    match &t.matcher {
        Matcher::NameSub { finder, folded } => {
            let needle = if *folded {
                finder.needle().to_vec()
            } else {
                fold_exact_needle(finder.needle())
            };
            (
                Driver::Sub {
                    needle_len: needle.len(),
                    finder: memmem::Finder::new(&needle).into_owned(),
                },
                *folded,
            )
        }
        Matcher::NamePrefix { bytes, folded } => (
            Driver::Prefix {
                bytes: if *folded {
                    bytes.clone()
                } else {
                    fold_exact_needle(bytes)
                },
            },
            *folded,
        ),
        Matcher::NameSuffix { bytes, folded } => (
            Driver::Suffixes {
                suffixes: vec![if *folded {
                    bytes.clone()
                } else {
                    fold_exact_needle(bytes)
                }],
                files_only: false,
            },
            *folded,
        ),
        Matcher::Ext { exts } => (
            Driver::Suffixes {
                suffixes: exts
                    .iter()
                    .map(|e| {
                        let mut s = Vec::with_capacity(e.len() + 1);
                        s.push(b'.');
                        s.extend_from_slice(e);
                        s
                    })
                    .collect(),
                files_only: true,
            },
            true,
        ),
        _ => unreachable!("driver_score gated"),
    }
}

/// Kill switch for the regex literal prefilter (`FMF_REGEX_PREFILTER=0`) —
/// forces literal-less *and* literal-bearing regex groups onto the chunked
/// full scan. A field escape hatch if a prefilter soundness bug ever
/// surfaces (the same shape as `FMF_QUERY_CACHE`, ADR-0023).
fn regex_prefilter_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("FMF_REGEX_PREFILTER").map_or(true, |v| v != "0"))
}

/// Extract a *required* literal factor from a name regex and turn it into a
/// folded-pool substring sweep — the same linear sweep every literal query
/// uses (ADR-0002), so regex stays off the full scan without any standing
/// index (ADR-0023).
///
/// Soundness: regex-syntax prefix (resp. suffix) extraction yields literals
/// that every match must begin (resp. end) with; the longest common
/// prefix/suffix `S` of that set is therefore present, contiguously, in
/// every matched substring — hence in the name. Folding `S` and sweeping the
/// (folded) lower pool is a superset for both case modes (an original-case
/// occurrence implies the folded one, length-preserving per code point), and
/// the `NameRegex` residual re-checks every candidate exactly. Returns `None`
/// when no usable literal exists (`\d+`, a leading `.*`, an alternation with
/// no common factor); the caller then falls back to a full scan.
fn regex_name_prefilter(re: &Regex) -> Option<Driver> {
    use regex_syntax::hir::literal::{ExtractKind, Extractor};

    let hir = regex_syntax::parse(re.as_str()).ok()?;
    let factor = |kind: ExtractKind| -> Option<Vec<u8>> {
        let is_suffix = matches!(kind, ExtractKind::Suffix);
        let mut ex = Extractor::new();
        ex.kind(kind);
        let seq = ex.extract(&hir);
        let bytes = if is_suffix {
            seq.longest_common_suffix()
        } else {
            seq.longest_common_prefix()
        }?;
        // A common factor that splits a multi-byte code point is unusable as
        // a folded needle; bail to a full scan rather than fold garbage.
        let folded = wtf8::fold_str(std::str::from_utf8(bytes).ok()?).into_bytes();
        // A 1-byte needle hits nearly every name — the per-hit sweep
        // bookkeeping then loses to a plain full scan (the `score >= 4` gate).
        (folded.len() >= 2).then_some(folded)
    };

    // Prefer the longer required factor; both map to a sound substring sweep.
    let needle = [ExtractKind::Prefix, ExtractKind::Suffix]
        .into_iter()
        .filter_map(factor)
        .max_by_key(Vec::len)?;
    Some(Driver::Sub {
        needle_len: needle.len(),
        finder: memmem::Finder::new(&needle).into_owned(),
    })
}

/// When a group has no literal driver, try to drive it from a positive name
/// regex's required literal. The regex matcher stays in `terms` as the
/// residual that confirms each candidate, so `driver_term` is `None`.
fn regex_prefilter_driver(terms: &[CTerm]) -> Driver {
    if !regex_prefilter_enabled() {
        return Driver::FullScan;
    }
    terms
        .iter()
        .filter(|t| !t.negated)
        .find_map(|t| match &t.matcher {
            Matcher::NameRegex { re } => regex_name_prefilter(re),
            _ => None,
        })
        .unwrap_or(Driver::FullScan)
}

/// Compile the *entire* query text as one regex (whole-query regex mode).
///
/// No parsing, no operators (ADR-0023) — the text is the pattern, matched
/// against the file name or the full path per `scope`. Name scope reuses the
/// literal prefilter; path scope falls back to a full scan (the path pool is
/// not contiguous). One AND group, the regex left as the residual.
///
/// # Errors
///
/// Returns [`CompileError::Regex`] if `text` is not a valid regex or exceeds
/// the compile size limit.
pub fn compile_whole_regex(
    text: &str,
    case: CaseMode,
    scope: RegexScope,
) -> Result<CompiledQuery, CompileError> {
    let re = build_regex(text, insensitive(text, case), text)?;
    let (matcher, needs_orig_paths) = match scope {
        RegexScope::Name => (Matcher::NameRegex { re }, false),
        RegexScope::Path => (Matcher::PathRegex { re }, true),
    };
    let term = CTerm {
        negated: false,
        matcher,
        exact_needle_unstable: false,
    };
    let driver = match scope {
        RegexScope::Name => regex_prefilter_driver(std::slice::from_ref(&term)),
        RegexScope::Path => Driver::FullScan,
    };
    Ok(CompiledQuery {
        groups: vec![CompiledGroup {
            driver,
            terms: vec![term],
            driver_term: None,
            driver_exact: true,
        }],
        needs_folded_paths: false,
        needs_orig_paths,
    })
}

/// Compile a parsed [`Ast`] into an executable [`CompiledQuery`].
///
/// # Errors
///
/// Returns [`CompileError::Regex`] if a `regex:`/`path:`-regex term fails to
/// compile.
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

        // Pick the most selective positive literal as the driver and pull it
        // out of the residual list. Empty needles (Matcher::True) never score.
        let mut driver_term = None;
        let mut driver_exact = true;
        let driver = if terms.is_empty() {
            Driver::MatchAll
        } else {
            let best = terms
                .iter()
                .enumerate()
                .filter_map(|(i, t)| driver_score(t).map(|s| (s, i)))
                .max_by_key(|(s, _)| *s);
            // Single-byte needles hit nearly every entry — the per-hit sweep
            // bookkeeping then costs more than a plain full scan does.
            match best {
                Some((score, i)) if score >= 4 => {
                    let t = terms.swap_remove(i);
                    let (d, exact) = driver_for(&t);
                    driver_term = Some(t);
                    driver_exact = exact;
                    d
                }
                // No usable literal driver. A positive name regex can still
                // narrow via its required literal (the regex stays a residual
                // — driver_term None, driver_exact irrelevant); otherwise full
                // scan.
                _ => regex_prefilter_driver(&terms),
            }
        };

        terms.sort_by_key(|t| t.matcher.cost());
        groups.push(CompiledGroup {
            driver,
            terms,
            driver_term,
            driver_exact,
        });
    }

    let needs_folded_paths = groups
        .iter()
        .flat_map(|g| &g.terms)
        .any(|t| t.matcher.needs_folded_path());
    let needs_orig_paths = groups
        .iter()
        .flat_map(|g| &g.terms)
        .any(|t| t.matcher.needs_orig_path());
    Ok(CompiledQuery {
        groups,
        needs_folded_paths,
        needs_orig_paths,
    })
}

impl CompiledQuery {
    /// Human-readable driver summary for `QueryTrace`.
    #[must_use]
    pub fn driver_label(&self) -> String {
        let mut labels: Vec<&str> = self.groups.iter().map(|g| g.driver.label()).collect();
        labels.dedup();
        labels.join("+")
    }
}

#[cfg(test)]
mod tests {
    use super::super::parse;
    use super::*;

    fn prefilter_needle(pattern: &str) -> Option<Vec<u8>> {
        let re = build_regex(pattern, false, pattern).unwrap();
        match regex_name_prefilter(&re) {
            Some(Driver::Sub { finder, .. }) => Some(finder.needle().to_vec()),
            Some(_) => panic!("regex prefilter must only produce a Sub driver"),
            None => None,
        }
    }

    #[test]
    fn regex_prefilter_extracts_required_literal() {
        // Leading literal → prefix factor.
        assert_eq!(prefilter_needle("^report"), Some(b"report".to_vec()));
        assert_eq!(prefilter_needle("windows.*"), Some(b"windows".to_vec()));
        // Trailing-anchored literal → suffix factor (the prefix is `.*`).
        assert_eq!(prefilter_needle(r".*\.dll"), Some(b".dll".to_vec()));
        assert_eq!(prefilter_needle(r"\.rs$"), Some(b".rs".to_vec()));
        // Folded for the lower-pool sweep; the case-sensitive residual still
        // re-checks each candidate.
        assert_eq!(prefilter_needle("^Report"), Some(b"report".to_vec()));
    }

    #[test]
    fn regex_prefilter_declines_without_a_usable_literal() {
        assert_eq!(prefilter_needle(r"\d+"), None);
        assert_eq!(prefilter_needle(".*"), None);
        assert_eq!(
            prefilter_needle("a"),
            None,
            "1-byte literal is not selective"
        );
        assert_eq!(
            prefilter_needle("dll|exe"),
            None,
            "no common factor across the alternation"
        );
    }

    #[test]
    fn regex_only_group_drives_a_pool_scan() {
        // A pure name-regex group with a literal must leave the full scan
        // behind: a Sub driver, no driver_term (the regex stays the residual).
        let ast = parse("regex:^report").unwrap();
        let q = compile(&ast, CaseMode::Smart, &super::super::dates::UtcResolver).unwrap();
        let g = &q.groups[0];
        assert!(matches!(g.driver, Driver::Sub { .. }), "expected pool-scan");
        assert!(g.driver_term.is_none(), "regex must remain a residual");
        assert!(
            g.terms
                .iter()
                .any(|t| matches!(t.matcher, Matcher::NameRegex { .. })),
            "the regex residual confirms each candidate"
        );

        // A literal-less regex stays on the full scan.
        let ast = parse(r"regex:\d+").unwrap();
        let q = compile(&ast, CaseMode::Smart, &super::super::dates::UtcResolver).unwrap();
        assert!(matches!(q.groups[0].driver, Driver::FullScan));
    }

    #[test]
    fn oversized_regex_is_rejected_not_compiled() {
        // A pattern that demands a >1 MiB program must come back as a clean
        // CompileError (→ FMF_E_QUERY_SYNTAX), never a panic or an OOM. The
        // bounded-repetition blowup unrolls past REGEX_SIZE_LIMIT.
        let ast = parse(r"regex:(a{500}){500}").unwrap();
        let result = compile(&ast, CaseMode::Smart, &super::super::dates::UtcResolver);
        assert!(
            matches!(result, Err(CompileError::Regex { .. })),
            "a 1 MiB+ regex program must be refused, not compiled"
        );
    }
}

#[cfg(test)]
mod proptests {
    use proptest::{prop_assert, proptest};

    use super::super::{dates::UtcResolver, parse};
    use super::*;

    proptest! {
        // Compiling a `regex:` term must never panic and never OOM, whatever
        // the pattern: it returns Ok (a built matcher) or a CompileError
        // (invalid syntax or over the size limit). Biased to the regex
        // metacharacter alphabet so the build paths get dense coverage.
        #[test]
        fn regex_compile_is_panic_free_and_bounded(
            body in r"[a-z0-9()\[\]{}.*+?^$|\\]{0,40}"
        ) {
            let text = format!("regex:\"{body}\"");
            if let Ok(ast) = parse(&text) {
                // Ok or Err — both are acceptable; the property is "no panic".
                let _ = compile(&ast, CaseMode::Smart, &UtcResolver);
                prop_assert!(true);
            }
        }
    }
}
