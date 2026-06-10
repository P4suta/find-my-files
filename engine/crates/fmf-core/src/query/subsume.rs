//! Query subsumption: does the result set of `next` provably fit inside the
//! result set of `prev`? When it does (and the index generation is
//! unchanged), the engine refines the cached `prev` ids instead of scanning
//! — the Everything-style incremental-typing fast path.
//!
//! Every rule here must be *sound* (false positives lose results — the one
//! unacceptable failure mode); being incomplete merely costs a cold scan.
//! The oracle property test in exec.rs cross-checks refine == fresh search
//! over random names and typing sequences.

use super::QueryOptions;
use super::compile::{CompiledQuery, Matcher};
use crate::wtf8;

/// True when every entry matching `next` (under `next_opt`) is guaranteed to
/// be present in `prev`'s materialized ids, in the right order.
pub(crate) fn subsumes(
    prev: &CompiledQuery,
    prev_opt: &QueryOptions,
    next: &CompiledQuery,
    next_opt: &QueryOptions,
) -> bool {
    // Cached ids are materialized in prev's sort order — refine preserves
    // subsequence order, so the sort must be identical.
    if prev_opt.sort != next_opt.sort || prev_opt.desc != next_opt.desc {
        return false;
    }
    // prev must have seen at least as much: hidden/system included before,
    // narrowed now is fine (refine re-filters); the reverse is not.
    if next_opt.include_hidden_system && !prev_opt.include_hidden_system {
        return false;
    }
    // CaseMode never needs comparing: its effect is baked into each compiled
    // matcher's `folded` flag, which the per-term rules check.

    // The empty query (single empty group) covers every live entry.
    let prev_is_match_all = prev.groups.len() == 1 && prev.groups[0].all_terms().next().is_none();
    if prev_is_match_all {
        return true;
    }
    // v1 keeps OR out of the implication algebra: one AND group each.
    let [prev_group] = prev.groups.as_slice() else {
        return false;
    };
    let [next_group] = next.groups.as_slice() else {
        return false;
    };

    // Every prev condition must be implied by some next condition; next's
    // extra terms only narrow an AND group further.
    prev_group.all_terms().all(|p| {
        next_group.all_terms().any(|n| {
            if p.negated != n.negated {
                return false;
            }
            if p.negated {
                // ¬A ⇒ ¬B only holds for B ⇒ A; equality is the safe core.
                matcher_eq(&p.matcher, &n.matcher)
            } else {
                implies(&n.matcher, &p.matcher)
            }
        }) || (!p.negated && matches!(p.matcher, Matcher::True))
    })
}

/// Needle-domain bridge: a byte needle proven about one pool implies one
/// about the other only in the orig→folded direction. Valid-UTF-8 needles
/// can only match at code-point boundaries and folding is length-preserving
/// per code point, so a name matching `n` at offset i guarantees the lower
/// pool holds `fold(n)` at i. The reverse (folded match → original bytes)
/// does not hold.
fn bridge_needle<'a>(
    n_bytes: &'a [u8],
    n_folded: bool,
    p_folded: bool,
) -> Option<std::borrow::Cow<'a, [u8]>> {
    match (n_folded, p_folded) {
        (true, true) | (false, false) => Some(std::borrow::Cow::Borrowed(n_bytes)),
        (false, true) => std::str::from_utf8(n_bytes)
            .ok()
            .map(|s| std::borrow::Cow::Owned(wtf8::fold_str(s).into_bytes())),
        (true, false) => None,
    }
}

/// Does a positive match of `n` guarantee a positive match of `p`?
fn implies(n: &Matcher, p: &Matcher) -> bool {
    use Matcher::*;
    match (n, p) {
        // Anything implies the always-true matcher.
        (_, True) => true,

        // Name literals: containment in the right pool domain.
        (
            NameSub {
                finder: nf,
                folded: nfo,
            },
            NameSub {
                finder: pf,
                folded: pfo,
            },
        ) => bridge_needle(nf.needle(), *nfo, *pfo)
            .is_some_and(|n| memchr::memmem::find(&n, pf.needle()).is_some()),
        // A prefix/suffix match still means "the name contains these bytes".
        (
            NamePrefix { bytes, folded: nfo },
            NameSub {
                finder: pf,
                folded: pfo,
            },
        )
        | (
            NameSuffix { bytes, folded: nfo },
            NameSub {
                finder: pf,
                folded: pfo,
            },
        ) => bridge_needle(bytes, *nfo, *pfo)
            .is_some_and(|n| memchr::memmem::find(&n, pf.needle()).is_some()),
        (
            NamePrefix {
                bytes: nb,
                folded: nfo,
            },
            NamePrefix {
                bytes: pb,
                folded: pfo,
            },
        ) => bridge_needle(nb, *nfo, *pfo).is_some_and(|n| n.starts_with(pb)),
        (
            NameSuffix {
                bytes: nb,
                folded: nfo,
            },
            NameSuffix {
                bytes: pb,
                folded: pfo,
            },
        ) => bridge_needle(nb, *nfo, *pfo).is_some_and(|n| n.ends_with(pb)),

        // Set/range narrowing. Ext semantics are *equality* on the extension,
        // so subset is the only sound relation ("ext:dl" never implies
        // "ext:d" — and never reaches here because {dl} ⊄ {d}).
        (Ext { exts: ne }, Ext { exts: pe }) => ne.iter().all(|e| pe.contains(e)),
        (
            Size {
                min: nmin,
                max: nmax,
            },
            Size {
                min: pmin,
                max: pmax,
            },
        ) => nmin >= pmin && nmax <= pmax,
        (
            Mtime {
                min: nmin,
                max: nmax,
            },
            Mtime {
                min: pmin,
                max: pmax,
            },
        ) => nmin >= pmin && nmax <= pmax,
        (IsDir(a), IsDir(b)) => a == b,

        // Path / regex terms: only exact equality is provable cheaply.
        _ => matcher_eq(n, p),
    }
}

/// Structural equality of two compiled matchers.
fn matcher_eq(a: &Matcher, b: &Matcher) -> bool {
    use Matcher::*;
    match (a, b) {
        (True, True) => true,
        (
            NameSub {
                finder: fa,
                folded: ca,
            },
            NameSub {
                finder: fb,
                folded: cb,
            },
        ) => ca == cb && fa.needle() == fb.needle(),
        (
            NamePrefix {
                bytes: ba,
                folded: ca,
            },
            NamePrefix {
                bytes: bb,
                folded: cb,
            },
        )
        | (
            NameSuffix {
                bytes: ba,
                folded: ca,
            },
            NameSuffix {
                bytes: bb,
                folded: cb,
            },
        ) => ca == cb && ba == bb,
        (
            PathSub {
                finder: fa,
                folded: ca,
            },
            PathSub {
                finder: fb,
                folded: cb,
            },
        ) => ca == cb && fa.needle() == fb.needle(),
        (NameRegex { re: ra }, NameRegex { re: rb })
        | (PathRegex { re: ra }, PathRegex { re: rb }) => ra.as_str() == rb.as_str(),
        (Ext { exts: ea }, Ext { exts: eb }) => ea == eb,
        (Size { min: ia, max: xa }, Size { min: ib, max: xb }) => ia == ib && xa == xb,
        (Mtime { min: ia, max: xa }, Mtime { min: ib, max: xb }) => ia == ib && xa == xb,
        (IsDir(a), IsDir(b)) => a == b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::super::dates::UtcResolver;
    use super::super::{CaseMode, compile, parse};
    use super::*;

    fn q(text: &str) -> CompiledQuery {
        compile(&parse(text).unwrap(), CaseMode::Smart, &UtcResolver).unwrap()
    }

    fn subsumed(prev: &str, next: &str) -> bool {
        let o = QueryOptions::default();
        subsumes(&q(prev), &o, &q(next), &o)
    }

    #[test]
    fn typing_extends_substring() {
        assert!(subsumed("", "w"));
        assert!(subsumed("w", "wi"));
        assert!(subsumed("wi", "win"));
        assert!(subsumed("win", "win .rs"));
        assert!(!subsumed("win", "wi"), "backspace must go cold");
        assert!(!subsumed("win", "wood"));
    }

    #[test]
    fn smart_case_bridges_orig_to_folded_only() {
        // "wi" (folded) ⊂ "Win" (orig, smart-case sensitive): a name
        // containing "Win" folds to a lower name containing "win" ⊇ "wi".
        assert!(subsumed("wi", "Win"));
        assert!(subsumed("Win", "Windows"), "same sensitive domain extends");
        // The reverse bridge is unsound: lower "win" ⊅ orig "Win".
        assert!(
            !subsumed("Win", "win"),
            "folded next cannot prove orig prev"
        );
    }

    #[test]
    fn filters_narrow_soundly() {
        assert!(subsumed("ext:rs;txt", "ext:rs"), "ext subset narrows");
        assert!(!subsumed("ext:d", "ext:dl"), "ext is equality, not prefix");
        assert!(!subsumed("ext:rs", "ext:rs;txt"), "superset widens");
        assert!(subsumed("size:>100", "size:>200"));
        assert!(!subsumed("size:>200", "size:>100"));
        assert!(subsumed("report", "report file:"), "added term narrows");
        assert!(!subsumed("report file:", "report"), "dropped term widens");
    }

    #[test]
    fn negation_requires_exact_match() {
        assert!(subsumed("rs !test", "rs !test"));
        assert!(subsumed("rs", "rs !test"), "added negation narrows");
        assert!(!subsumed("rs !test", "rs"), "dropped negation widens");
        assert!(
            !subsumed("rs !test", "rs !tes"),
            "¬tes does not imply ¬test"
        );
    }

    #[test]
    fn or_and_option_changes_go_cold() {
        assert!(!subsumed("a | b", "ab"), "OR prev is out of the v1 algebra");
        assert!(!subsumed("ab", "ab | cd"));
        assert!(subsumed("", "ab | cd"), "match-all subsumes even an OR");

        let prev = q("win");
        let next = q("wind");
        let base = QueryOptions::default();
        let desc = QueryOptions { desc: true, ..base };
        assert!(!subsumes(&prev, &base, &next, &desc), "sort flip");
        let hidden = QueryOptions {
            include_hidden_system: true,
            ..base
        };
        assert!(!subsumes(&prev, &base, &next, &hidden), "widening toggle");
        assert!(subsumes(&prev, &hidden, &next, &base), "narrowing toggle");
    }

    #[test]
    fn wildcard_and_path_terms_need_equality() {
        assert!(subsumed("*.rs", "*.rs"));
        assert!(subsumed("*.rs", "*.rs main"), "extra term narrows");
        // ".rs" suffix-implies the substring "rs" — sound and allowed:
        assert!(subsumed("rs", "*.rs"));
        // …but a *different* wildcard never implies another one.
        assert!(!subsumed("*.rsx", "*.rs"));
        assert!(subsumed(r"path:src", r"path:src main"));
        assert!(
            !subsumed(r"path:src", r"path:srcmain"),
            "path needs equality"
        );
    }
}
