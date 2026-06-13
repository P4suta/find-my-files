use super::compile::{CTerm, Matcher};
use super::memo::PathMemos;
use crate::index::{EntryId, VolumeIndex};

// ── Residual matcher evaluation ─────────────────────────────────────────

/// Per-thread scratch: the entry's full path, built at most once per entry
/// per variant, only when a path matcher is actually reached.
#[derive(Default)]
pub(super) struct EvalCtx {
    lower_path: Vec<u8>,
    orig_path: Vec<u8>,
    lower_built: bool,
    orig_built: bool,
}

impl EvalCtx {
    #[inline]
    const fn reset(&mut self) {
        self.lower_built = false;
        self.orig_built = false;
    }

    #[inline]
    fn lower_path<'a>(&'a mut self, idx: &VolumeIndex, memo: &PathMemos, id: EntryId) -> &'a [u8] {
        if !self.lower_built {
            self.lower_path.clear();
            if id != VolumeIndex::ROOT {
                self.lower_path
                    .extend_from_slice(memo.lower_prefix(idx.parent(id)));
            }
            self.lower_path.extend_from_slice(idx.lower_name(id));
            self.lower_built = true;
        }
        &self.lower_path
    }

    #[inline]
    fn orig_path<'a>(&'a mut self, idx: &VolumeIndex, memo: &PathMemos, id: EntryId) -> &'a [u8] {
        if !self.orig_built {
            self.orig_path.clear();
            if id != VolumeIndex::ROOT {
                self.orig_path
                    .extend_from_slice(memo.orig_prefix(idx.parent(id)));
            }
            self.orig_path.extend_from_slice(idx.name(id));
            self.orig_built = true;
        }
        &self.orig_path
    }
}

/// The haystack for a case-exact name literal. Fold-identical entries
/// resolve in O(1) (ADR-0004): a needle that is not its own fold can never
/// occur in a name whose every character is fold-stable (UTF-8/WTF-8
/// self-synchronization makes the byte-level argument sound), and for a
/// fold-stable needle the folded bytes *are* the original bytes.
#[inline]
fn exact_hay<'a>(idx: &'a VolumeIndex, t: &CTerm, id: EntryId) -> Option<&'a [u8]> {
    if idx.is_fold_identical(id) {
        if t.exact_needle_unstable {
            None
        } else {
            Some(idx.lower_name(id))
        }
    } else {
        Some(idx.name(id))
    }
}

#[inline]
fn eval(idx: &VolumeIndex, memo: &PathMemos, ctx: &mut EvalCtx, t: &CTerm, id: EntryId) -> bool {
    match &t.matcher {
        Matcher::True => true,
        Matcher::Size { min, max } => !idx.is_dir(id) && (*min..=*max).contains(&idx.size(id)),
        Matcher::Mtime { min, max } => (*min..=*max).contains(&idx.mtime(id)),
        Matcher::IsDir(d) => idx.is_dir(id) == *d,
        Matcher::Ext { exts } => {
            let lower = idx.lower_name(id);
            match memchr::memrchr(b'.', lower) {
                Some(p) if !idx.is_dir(id) => {
                    let ext = &lower[p + 1..];
                    exts.iter().any(|e| e.as_slice() == ext)
                }
                _ => false,
            }
        }
        Matcher::NameSub { finder, folded } => {
            let hay = if *folded {
                idx.lower_name(id)
            } else {
                match exact_hay(idx, t, id) {
                    Some(h) => h,
                    None => return false,
                }
            };
            finder.find(hay).is_some()
        }
        Matcher::NamePrefix { bytes, folded } => {
            let hay = if *folded {
                idx.lower_name(id)
            } else {
                match exact_hay(idx, t, id) {
                    Some(h) => h,
                    None => return false,
                }
            };
            hay.starts_with(bytes)
        }
        Matcher::NameSuffix { bytes, folded } => {
            let hay = if *folded {
                idx.lower_name(id)
            } else {
                match exact_hay(idx, t, id) {
                    Some(h) => h,
                    None => return false,
                }
            };
            hay.ends_with(bytes)
        }
        Matcher::NameRegex { re } => re.is_match(idx.name(id)),
        Matcher::PathSub { finder, folded } => {
            let hay = if *folded {
                ctx.lower_path(idx, memo, id)
            } else {
                ctx.orig_path(idx, memo, id)
            };
            finder.find(hay).is_some()
        }
        Matcher::PathRegex { re } => re.is_match(ctx.orig_path(idx, memo, id)),
    }
}

#[inline]
pub(super) fn terms_match(
    idx: &VolumeIndex,
    memo: &PathMemos,
    ctx: &mut EvalCtx,
    terms: &[CTerm],
    id: EntryId,
) -> bool {
    terms_match_iter(idx, memo, ctx, terms.iter(), id)
}

/// Iterator form so refine can chain the driver term with the residuals
/// without cloning matchers (`CompiledGroup::all_terms`).
#[inline]
pub(super) fn terms_match_iter<'a>(
    idx: &VolumeIndex,
    memo: &PathMemos,
    ctx: &mut EvalCtx,
    terms: impl Iterator<Item = &'a CTerm>,
    id: EntryId,
) -> bool {
    ctx.reset();
    for t in terms {
        if eval(idx, memo, ctx, t, id) == t.negated {
            return false;
        }
    }
    true
}
