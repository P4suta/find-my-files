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
        canonical: bool,
    },
    /// Name starts with the bytes (`lit*`).
    NamePrefix {
        bytes: Vec<u8>,
        folded: bool,
        canonical: bool,
    },
    /// Name ends with the bytes (`*.lit`).
    NameSuffix {
        bytes: Vec<u8>,
        folded: bool,
        canonical: bool,
    },
    /// Substring in the full path.
    PathSub {
        finder: memmem::Finder<'static>,
        folded: bool,
        canonical: bool,
    },
    /// Anchored wildcard or user regex over the name bytes. Ordinary
    /// wildcards set `canonical`; explicit regex syntax deliberately does not.
    NameRegex {
        re: Wtf8Regex,
        canonical: bool,
    },
    /// Unanchored wildcard/regex over full-path bytes.
    PathRegex {
        re: Wtf8Regex,
        canonical: bool,
    },
    /// Extension equals any of these folded byte strings.
    Ext {
        exts: Vec<Vec<u8>>,
        canonical: bool,
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

/// A Unicode regex for ordinary names plus a mixed Unicode/WTF-8 fallback for
/// legal NTFS names that contain a lone surrogate.
///
/// Making the only regex byte-oriented would let it traverse lone-surrogate
/// bytes, but would also change `.`/`?` from one Unicode scalar to one byte and
/// disable Unicode-aware case matching for every normal file name. Keep the
/// existing semantics on valid UTF-8. The fallback keeps the surrounding
/// expression Unicode-aware and makes only its any-code-point atoms byte-aware;
/// it is consulted only for the representation the primary cannot consume.
pub(super) struct Wtf8Regex {
    unicode: Regex,
    wtf8: Regex,
    case_insensitive: bool,
}

impl Wtf8Regex {
    #[inline]
    pub(super) fn is_match(&self, haystack: &[u8]) -> bool {
        if self.unicode.is_match(haystack) {
            return true;
        }
        has_lone_surrogate(haystack) && self.wtf8.is_match(haystack)
    }

    #[inline]
    pub(super) fn as_str(&self) -> &str {
        self.unicode.as_str()
    }

    pub(super) fn same_pattern(&self, other: &Self) -> bool {
        self.case_insensitive == other.case_insensitive
            && self.unicode.as_str() == other.unicode.as_str()
            && self.wtf8.as_str() == other.wtf8.as_str()
    }
}

/// The index stores well-formed WTF-8, whose only departure from valid UTF-8 is
/// ED A0..BF 80..BF (a UTF-16 surrogate). Avoid a second full UTF-8 decoder
/// pass on every ordinary regex miss; memchr makes the overwhelmingly common
/// "no ED byte" case a vectorized scan.
fn has_lone_surrogate(bytes: &[u8]) -> bool {
    let mut rest = bytes;
    while let Some(relative) = memchr::memchr(0xED, rest) {
        let lead = &rest[relative..];
        if lead.get(1..3).is_some_and(|tail| {
            (0xA0..=0xBF).contains(&tail[0]) && (0x80..=0xBF).contains(&tail[1])
        }) {
            return true;
        }
        rest = &lead[1..];
    }
    false
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
        canonical: bool,
    },
    Prefix {
        bytes: Vec<u8>,
        canonical: bool,
    },
    Suffixes {
        suffixes: Vec<Vec<u8>>,
        files_only: bool,
        canonical: bool,
    },
}

impl Driver {
    pub(super) const fn label(&self) -> &'static str {
        match self {
            Self::FullScan => "full-scan",
            Self::MatchAll => "match-all",
            Self::Sub {
                canonical: true, ..
            }
            | Self::Prefix {
                canonical: true, ..
            }
            | Self::Suffixes {
                canonical: true, ..
            } => "canonical-scan",
            Self::Sub { .. } => "pool-scan",
            Self::Prefix { .. } => "prefix",
            Self::Suffixes { .. } => "suffix",
        }
    }

    pub(super) const fn canonical(&self) -> bool {
        match self {
            Self::Sub { canonical, .. }
            | Self::Prefix { canonical, .. }
            | Self::Suffixes { canonical, .. } => *canonical,
            Self::FullScan | Self::MatchAll => false,
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
    /// False when the folded sweep is only a superset: case-exact terms and
    /// canonical completion both require `driver_term` verification.
    pub driver_exact: bool,
}

impl CompiledGroup {
    /// Every condition of this AND group: the driver's source term (most
    /// selective, so first) followed by the cost-ordered residuals.
    pub(super) fn all_terms(&self) -> impl Iterator<Item = &CTerm> {
        self.driver_term.iter().chain(self.terms.iter())
    }

    /// The conditions the sweep did *not* fully check: the residuals, plus
    /// the driver's source term when candidate generation was a superset.
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

/// Smart-case decision for regex syntax, whose spelling is intentionally not
/// normalized.
fn regex_insensitive(needle: &str, case: CaseMode) -> bool {
    match case {
        CaseMode::Insensitive => true,
        CaseMode::Sensitive => false,
        CaseMode::Smart => !wtf8::has_uppercase(needle),
    }
}

/// Smart-case decision for ordinary text. Canonically equivalent query
/// spellings must choose the same case domain (for example `K` and `K`).
fn literal_insensitive(needle: &str, case: CaseMode) -> bool {
    match case {
        CaseMode::Insensitive => true,
        CaseMode::Sensitive => false,
        CaseMode::Smart if needle.is_ascii() => !wtf8::has_uppercase(needle),
        CaseMode::Smart => {
            let canonical = wtf8::normalize_str(needle, false);
            let canonical = std::str::from_utf8(&canonical)
                .expect("normalizing a query string preserves valid UTF-8");
            !wtf8::has_uppercase(canonical)
        }
    }
}

/// NFC leaves ASCII unchanged, but three non-ASCII characters have canonical
/// singleton decompositions into ASCII in the locked Unicode table: Greek
/// question mark → `;`, Greek varia → `` ` ``, and Kelvin sign → `K`. Those
/// literals (`k` too in a folded domain) must inspect non-ASCII spellings.
fn needs_canonical_view(needle: &str, folded: bool) -> bool {
    !needle.is_ascii()
        || needle.bytes().any(|b| {
            b == b';'
                || b == b'`'
                || if folded {
                    b.eq_ignore_ascii_case(&b'k')
                } else {
                    b == b'K'
                }
        })
}

fn fold_needle(needle: &str, case: CaseMode) -> (Vec<u8>, bool, bool) {
    let folded = literal_insensitive(needle, case);
    let canonical = needs_canonical_view(needle, folded);
    if canonical {
        (wtf8::normalize_str(needle, folded), folded, true)
    } else if folded {
        (wtf8::fold_str(needle).into_bytes(), true, false)
    } else {
        (needle.as_bytes().to_vec(), false, false)
    }
}

fn substring_finder(needle: &str, case: CaseMode) -> (memmem::Finder<'static>, bool, bool) {
    let (bytes, folded, canonical) = fold_needle(needle, case);
    (memmem::Finder::new(&bytes).into_owned(), folded, canonical)
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

// The ordinary dot keeps the caller's Unicode/newline flags. Its local byte
// alternative adds exactly the one code-point range UTF-8 excludes but WTF-8
// admits: an encoded UTF-16 surrogate.
const WTF8_DOT: &str = r"(?:.|(?-u:\xED[\xA0-\xBF][\x80-\xBF]))";

fn wildcard_to_wtf8_regex_body(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len() * 2);
    for c in pattern.chars() {
        match c {
            '*' => {
                out.push_str(WTF8_DOT);
                out.push('*');
            }
            '?' => out.push_str(WTF8_DOT),
            c => out.push_str(&regex::escape(&c.to_string())),
        }
    }
    out
}

/// Replace regex dot atoms outside character classes with one canonical
/// WTF-8 code point. The surrounding pattern stays in Unicode mode, so
/// literals, Unicode classes, and case folding retain their normal semantics;
/// only this atom locally admits lone-surrogate bytes.
fn regex_to_wtf8_fallback(body: &str) -> String {
    use regex_syntax::ast::{Ast, parse::Parser};

    fn replace_dots(ast: &mut Ast, replacement: &Ast) {
        match ast {
            Ast::Dot(_) => *ast = replacement.clone(),
            Ast::Repetition(repetition) => replace_dots(&mut repetition.ast, replacement),
            Ast::Group(group) => replace_dots(&mut group.ast, replacement),
            Ast::Alternation(alternation) => {
                for child in &mut alternation.asts {
                    replace_dots(child, replacement);
                }
            }
            Ast::Concat(concat) => {
                for child in &mut concat.asts {
                    replace_dots(child, replacement);
                }
            }
            _ => {}
        }
    }

    let Ok(mut ast) = Parser::new().parse(body) else {
        return body.to_string();
    };
    let replacement = Parser::new()
        .parse(WTF8_DOT)
        .expect("the static WTF-8 atom is valid regex syntax");
    replace_dots(&mut ast, &replacement);
    ast.to_string()
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

fn regex_builder(body: &str, ci: bool) -> RegexBuilder {
    let mut builder = RegexBuilder::new(body);
    builder
        .case_insensitive(ci)
        .dot_matches_new_line(true)
        .size_limit(REGEX_SIZE_LIMIT)
        .dfa_size_limit(REGEX_DFA_SIZE_LIMIT);
    builder
}

fn build_regex_with_wtf8_fallback(
    body: &str,
    wtf8_body: &str,
    ci: bool,
    pattern_for_err: &str,
) -> Result<Wtf8Regex, CompileError> {
    let unicode = regex_builder(body, ci)
        .build()
        .map_err(|source| CompileError::Regex {
            pattern: pattern_for_err.to_string(),
            source,
        })?;
    // A query has one meaning for every legal NTFS name. If the expanded WTF-8
    // program exceeds the same hard compilation bound, reject the whole query
    // instead of silently returning incomplete results for lone-surrogate names.
    let wtf8 = regex_builder(wtf8_body, ci)
        .build()
        .map_err(|source| CompileError::Regex {
            pattern: pattern_for_err.to_string(),
            source,
        })?;
    Ok(Wtf8Regex {
        unicode,
        wtf8,
        case_insensitive: ci,
    })
}

fn build_regex(body: &str, ci: bool, pattern_for_err: &str) -> Result<Wtf8Regex, CompileError> {
    let wtf8_body = regex_to_wtf8_fallback(body);
    build_regex_with_wtf8_fallback(body, &wtf8_body, ci, pattern_for_err)
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
            let (finder, folded, canonical) = substring_finder(s, case);
            Matcher::NameSub {
                finder,
                folded,
                canonical,
            }
        }
        Term::Path(s) => {
            let (finder, folded, canonical) = substring_finder(s, case);
            Matcher::PathSub {
                finder,
                folded,
                canonical,
            }
        }
        Term::Wildcard(s) => match classify_wildcard(s) {
            WildShape::Prefix(lit) => {
                let (bytes, folded, canonical) = fold_needle(&lit, case);
                Matcher::NamePrefix {
                    bytes,
                    folded,
                    canonical,
                }
            }
            WildShape::Suffix(lit) => {
                let (bytes, folded, canonical) = fold_needle(&lit, case);
                Matcher::NameSuffix {
                    bytes,
                    folded,
                    canonical,
                }
            }
            WildShape::Inner(lit) => {
                let (finder, folded, canonical) = substring_finder(&lit, case);
                Matcher::NameSub {
                    finder,
                    folded,
                    canonical,
                }
            }
            WildShape::General => {
                let folded = literal_insensitive(s, case);
                let canonical = needs_canonical_view(s, folded);
                let pattern = if canonical {
                    String::from_utf8(wtf8::normalize_str(s, false))
                        .expect("a normalized query pattern remains valid UTF-8")
                } else {
                    s.clone()
                };
                let body = format!("^{}$", wildcard_to_regex_body(&pattern));
                let wtf8_body = format!("^{}$", wildcard_to_wtf8_regex_body(&pattern));
                Matcher::NameRegex {
                    re: build_regex_with_wtf8_fallback(&body, &wtf8_body, folded, s)?,
                    canonical,
                }
            }
        },
        Term::PathWildcard(s) => {
            let folded = literal_insensitive(s, case);
            let canonical = needs_canonical_view(s, folded);
            let pattern = if canonical {
                String::from_utf8(wtf8::normalize_str(s, false))
                    .expect("a normalized query pattern remains valid UTF-8")
            } else {
                s.clone()
            };
            let body = wildcard_to_regex_body(&pattern);
            let wtf8_body = wildcard_to_wtf8_regex_body(&pattern);
            Matcher::PathRegex {
                re: build_regex_with_wtf8_fallback(&body, &wtf8_body, folded, s)?,
                canonical,
            }
        }
        Term::Regex(s) => Matcher::NameRegex {
            re: build_regex(s, regex_insensitive(s, case), s)?,
            canonical: false,
        },
        Term::Ext(exts) => {
            let canonical = exts.iter().any(|e| needs_canonical_view(e, true));
            Matcher::Ext {
                exts: exts
                    .iter()
                    .map(|e| {
                        if canonical {
                            wtf8::normalize_str(e, true)
                        } else {
                            wtf8::fold_str(e).into_bytes()
                        }
                    })
                    .collect(),
                canonical,
            }
        }
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
            ..
        } => unstable(finder.needle()),
        Matcher::NamePrefix {
            bytes,
            folded: false,
            ..
        }
        | Matcher::NameSuffix {
            bytes,
            folded: false,
            ..
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
        Matcher::Ext { exts, .. } if !exts.is_empty() => {
            Some(exts.iter().map(|e| (e.len() + 1) * 2).min().unwrap_or(0))
        }
        _ => None,
    }
}

/// Fold a case-exact needle for the superset sweep. Needles always
/// originate from the query `&str`, so the bytes are valid UTF-8; the
/// fold's length preservation keeps prefix/suffix anchors sound.
fn fold_exact_needle(bytes: &[u8], canonical: bool) -> Vec<u8> {
    let s = std::str::from_utf8(bytes).expect("query needles are valid UTF-8");
    if canonical {
        wtf8::normalize_str(s, true)
    } else {
        wtf8::fold_str(s).into_bytes()
    }
}

/// Build the sweep driver from a term, leaving the term intact (kept as
/// `CompiledGroup::driver_term`). Returns the driver and whether it fully
/// checks the term. Case-exact terms fold their needle, and canonical terms
/// union raw and normalized spellings; both are sound supersets whose source
/// matcher runs again as a residual.
fn driver_for(t: &CTerm) -> (Driver, bool) {
    match &t.matcher {
        Matcher::NameSub {
            finder,
            folded,
            canonical,
        } => {
            let needle = if *folded {
                finder.needle().to_vec()
            } else {
                fold_exact_needle(finder.needle(), *canonical)
            };
            (
                Driver::Sub {
                    needle_len: needle.len(),
                    finder: memmem::Finder::new(&needle).into_owned(),
                    canonical: *canonical,
                },
                *folded && !*canonical,
            )
        }
        Matcher::NamePrefix {
            bytes,
            folded,
            canonical,
        } => (
            Driver::Prefix {
                bytes: if *folded {
                    bytes.clone()
                } else {
                    fold_exact_needle(bytes, *canonical)
                },
                canonical: *canonical,
            },
            *folded && !*canonical,
        ),
        Matcher::NameSuffix {
            bytes,
            folded,
            canonical,
        } => (
            Driver::Suffixes {
                suffixes: vec![if *folded {
                    bytes.clone()
                } else {
                    fold_exact_needle(bytes, *canonical)
                }],
                files_only: false,
                canonical: *canonical,
            },
            *folded && !*canonical,
        ),
        Matcher::Ext { exts, canonical } => {
            // The suffix sweep exactly implements extension equality only when
            // every value is one final path segment.  For example,
            // `ext:txt.PDF` produces the useful candidate suffix `.txt.PDF`,
            // but a name ending in it has extension `PDF`, not `txt.PDF`.
            // Keep that candidate plan and re-run the source matcher.
            let suffix_is_exact = exts.iter().all(|e| !e.contains(&b'.'));
            (
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
                    canonical: *canonical,
                },
                !*canonical && suffix_is_exact,
            )
        }
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
fn regex_name_prefilter(re: &Wtf8Regex) -> Option<Driver> {
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
        canonical: false,
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
            Matcher::NameRegex {
                re,
                canonical: false,
            } => regex_name_prefilter(re),
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
    let re = build_regex(text, regex_insensitive(text, case), text)?;
    let (matcher, needs_orig_paths) = match scope {
        RegexScope::Name => (
            Matcher::NameRegex {
                re,
                canonical: false,
            },
            false,
        ),
        RegexScope::Path => (
            Matcher::PathRegex {
                re,
                canonical: false,
            },
            true,
        ),
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
    use unicode_normalization::UnicodeNormalization;

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
    fn ascii_canonical_alias_trigger_covers_the_locked_unicode_table() {
        // `needs_canonical_view` keeps every other ASCII literal on the raw
        // hot path. Pin that optimization to the exact Unicode table shipped
        // by the locked normalization crate instead of reasoning only from
        // decomposition metadata: inspect each scalar's actual NFC output.
        let mut aliases = Vec::new();
        for cp in 0x80..=char::MAX as u32 {
            let Some(c) = char::from_u32(cp) else {
                continue;
            };
            let normalized: String = c.to_string().nfc().collect();
            let ascii: String = normalized.chars().filter(char::is_ascii).collect();
            if !ascii.is_empty() {
                aliases.push((c, ascii));
            }
        }
        assert_eq!(
            aliases,
            vec![
                ('\u{037e}', ";".to_string()),
                ('\u{1fef}', "`".to_string()),
                ('\u{212a}', "K".to_string())
            ]
        );

        for b in 0u8..=0x7F {
            let needle = char::from(b).to_string();
            assert_eq!(
                needs_canonical_view(&needle, false),
                matches!(b, b';' | b'`' | b'K'),
                "case-exact ASCII trigger drifted for 0x{b:02X}"
            );
            assert_eq!(
                needs_canonical_view(&needle, true),
                matches!(b, b';' | b'`' | b'K' | b'k'),
                "folded ASCII trigger drifted for 0x{b:02X}"
            );
        }
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
    fn literal_and_extension_driver_plans_pin_selectivity_and_exactness() {
        let resolver = super::super::dates::UtcResolver;

        let two_byte = compile(&parse("ab").unwrap(), CaseMode::Smart, &resolver).unwrap();
        let group = &two_byte.groups[0];
        assert!(matches!(group.driver, Driver::Sub { .. }));
        assert!(group.driver_term.is_some());
        assert!(group.driver_exact);

        for text in ["a", "!ab"] {
            let query = compile(&parse(text).unwrap(), CaseMode::Smart, &resolver).unwrap();
            let group = &query.groups[0];
            assert!(matches!(group.driver, Driver::FullScan), "{text:?}");
            assert!(group.driver_term.is_none(), "{text:?}");
        }

        let extension = compile(&parse("ext:rs").unwrap(), CaseMode::Smart, &resolver).unwrap();
        let group = &extension.groups[0];
        match &group.driver {
            Driver::Suffixes {
                suffixes,
                files_only,
                canonical,
            } => {
                assert_eq!(suffixes, &[b".rs".to_vec()]);
                assert!(*files_only);
                assert!(!*canonical);
            }
            _ => panic!("expected extension suffix driver"),
        }
        assert!(group.driver_term.is_some());
        assert!(group.driver_exact);

        let dotted = compile(&parse("ext:txt.PDF").unwrap(), CaseMode::Smart, &resolver).unwrap();
        let group = &dotted.groups[0];
        assert!(matches!(group.driver, Driver::Suffixes { .. }));
        assert!(group.driver_term.is_some());
        assert!(
            !group.driver_exact,
            "a dotted value is not a final extension"
        );

        let empty_extension = compile(&parse("ext:").unwrap(), CaseMode::Smart, &resolver).unwrap();
        let group = &empty_extension.groups[0];
        assert!(matches!(group.driver, Driver::FullScan));
        assert_eq!(group.terms.len(), 1);
        assert_eq!(driver_score(&group.terms[0]), None);
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

    #[test]
    fn wtf8_fallback_compile_failure_rejects_the_whole_query() {
        let result = build_regex_with_wtf8_fallback("ordinary", "(", false, "ordinary");
        assert!(
            matches!(result, Err(CompileError::Regex { .. })),
            "a missing fallback must never silently narrow results"
        );
    }

    #[test]
    fn regex_identity_includes_fallback_and_case_mode() {
        let base = build_regex_with_wtf8_fallback("^A.B$", "^A.B$", false, "^A.B$").unwrap();
        let different_fallback =
            build_regex_with_wtf8_fallback("^A.B$", "^A(?:.)B$", false, "^A.B$").unwrap();
        let different_case =
            build_regex_with_wtf8_fallback("^A.B$", "^A.B$", true, "^A.B$").unwrap();

        assert!(!base.same_pattern(&different_fallback));
        assert!(!base.same_pattern(&different_case));
    }

    #[test]
    fn regex_keeps_unicode_semantics_and_can_cross_a_lone_surrogate() {
        let one_scalar = build_regex(r"^.$", false, r"^.$").unwrap();
        assert!(
            one_scalar.is_match("日".as_bytes()),
            "normal names keep Unicode-scalar dot semantics"
        );
        let escaped_dot = build_regex(r"^\.$", false, r"^\.$").unwrap();
        let class_dot = build_regex(r"^[.]$", false, r"^[.]$").unwrap();
        assert!(escaped_dot.is_match(b"."));
        assert!(class_dot.is_match(b"."));

        let case_insensitive = build_regex(r"^Σ$", true, r"^Σ$").unwrap();
        assert!(
            case_insensitive.is_match("σ".as_bytes()),
            "normal names keep Unicode-aware case matching"
        );

        // "A<lone high surrogate>B" in canonical WTF-8. The primary Unicode
        // regex cannot traverse ED A0 80, but the mixed fallback can.
        let wtf8 = [b'A', 0xED, 0xA0, 0x80, b'B'];
        assert!(!escaped_dot.is_match(&wtf8[1..4]));
        assert!(!class_dot.is_match(&wtf8[1..4]));
        assert!(has_lone_surrogate(&wtf8));
        assert!(
            !has_lone_surrogate(&[0xED, 0x9F, 0xBF]),
            "the valid scalar immediately below the surrogate range"
        );
        let spanning = build_regex(r"^A.*B$", false, r"^A.*B$").unwrap();
        assert!(
            spanning.is_match(&wtf8),
            "a legal NTFS name must not become invisible to regex"
        );
        assert!(
            one_scalar.is_match(&wtf8[1..4]),
            "raw regex dot must consume one lone-surrogate code point"
        );

        let mut greek_wtf8 = "σ".as_bytes().to_vec();
        greek_wtf8.extend_from_slice(&wtf8[1..]);
        let unicode_case_across_surrogate = build_regex(r"^Σ.*B$", true, r"^Σ.*B$").unwrap();
        assert!(
            unicode_case_across_surrogate.is_match(&greek_wtf8),
            "fallback must retain Unicode case folding around a surrogate"
        );

        let wildcard = build_regex_with_wtf8_fallback(
            &format!("^{}$", wildcard_to_regex_body("A?B")),
            &format!("^{}$", wildcard_to_wtf8_regex_body("A?B")),
            false,
            "A?B",
        )
        .unwrap();
        assert!(
            wildcard.is_match(&wtf8),
            "wildcard ? must consume one WTF-8 code point, not one byte"
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
