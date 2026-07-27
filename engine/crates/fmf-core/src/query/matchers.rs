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
    canonical_lower_path: Vec<u8>,
    canonical_orig_path: Vec<u8>,
    canonical_lower_name: Vec<u8>,
    canonical_orig_name: Vec<u8>,
    path_chain: Vec<EntryId>,
    lower_built: bool,
    orig_built: bool,
    canonical_lower_path_built: bool,
    canonical_orig_path_built: bool,
    canonical_lower_name_built: bool,
    canonical_orig_name_built: bool,
}

impl EvalCtx {
    #[inline]
    const fn reset(&mut self) {
        self.lower_built = false;
        self.orig_built = false;
        self.canonical_lower_path_built = false;
        self.canonical_orig_path_built = false;
        self.canonical_lower_name_built = false;
        self.canonical_orig_name_built = false;
    }

    #[inline]
    fn lower_path<'a>(&'a mut self, idx: &VolumeIndex, memo: &PathMemos, id: EntryId) -> &'a [u8] {
        if !self.lower_built {
            self.lower_path.clear();
            if id != VolumeIndex::ROOT {
                memo.append_lower_parent(idx, id, &mut self.lower_path, &mut self.path_chain);
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
                memo.append_orig_parent(idx, id, &mut self.orig_path, &mut self.path_chain);
            }
            self.orig_path.extend_from_slice(idx.name(id));
            self.orig_built = true;
        }
        &self.orig_path
    }

    #[inline]
    fn canonical_lower_path<'a>(
        &'a mut self,
        idx: &VolumeIndex,
        memo: &PathMemos,
        id: EntryId,
    ) -> &'a [u8] {
        // Canonicalize before folding. Those operations do not commute for
        // every scalar sequence (for example `I` + U+0307 versus U+0130), so
        // the storage-folded path is not a sound source for this view.
        let original_is_ascii = {
            let original = self.orig_path(idx, memo, id);
            original.is_ascii()
        };
        if original_is_ascii {
            return self.lower_path(idx, memo, id);
        }
        if !self.canonical_lower_path_built {
            crate::wtf8::normalize_wtf8_into(&self.orig_path, true, &mut self.canonical_lower_path);
            self.canonical_lower_path_built = true;
        }
        &self.canonical_lower_path
    }

    #[inline]
    fn canonical_orig_path<'a>(
        &'a mut self,
        idx: &VolumeIndex,
        memo: &PathMemos,
        id: EntryId,
    ) -> &'a [u8] {
        let _ = self.orig_path(idx, memo, id);
        if self.orig_path.is_ascii() {
            return &self.orig_path;
        }
        if !self.canonical_orig_path_built {
            crate::wtf8::normalize_wtf8_into(&self.orig_path, false, &mut self.canonical_orig_path);
            self.canonical_orig_path_built = true;
        }
        &self.canonical_orig_path
    }

    #[inline]
    fn canonical_lower_name<'a>(&'a mut self, idx: &'a VolumeIndex, id: EntryId) -> &'a [u8] {
        let original = idx.name(id);
        if original.is_ascii() {
            return idx.lower_name(id);
        }
        if !self.canonical_lower_name_built {
            // Normalize the original spelling before folding. Re-normalizing
            // the stored lower pool would lose canonical equivalence where
            // composition changes the length-preserving fold decision.
            crate::wtf8::normalize_wtf8_into(original, true, &mut self.canonical_lower_name);
            self.canonical_lower_name_built = true;
        }
        &self.canonical_lower_name
    }

    #[inline]
    fn canonical_orig_name<'a>(&'a mut self, idx: &'a VolumeIndex, id: EntryId) -> &'a [u8] {
        let original = idx.name(id);
        if original.is_ascii() {
            return original;
        }
        if !self.canonical_orig_name_built {
            crate::wtf8::normalize_wtf8_into(original, false, &mut self.canonical_orig_name);
            self.canonical_orig_name_built = true;
        }
        &self.canonical_orig_name
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
        Matcher::Ext { exts, canonical } => {
            let lower = if *canonical {
                ctx.canonical_lower_name(idx, id)
            } else {
                idx.lower_name(id)
            };
            match memchr::memrchr(b'.', lower) {
                Some(p) if !idx.is_dir(id) => {
                    let ext = &lower[p + 1..];
                    exts.iter().any(|e| e.as_slice() == ext)
                }
                _ => false,
            }
        }
        Matcher::NameSub {
            finder,
            folded,
            canonical,
        } => {
            let hay = if *canonical {
                if *folded {
                    ctx.canonical_lower_name(idx, id)
                } else {
                    ctx.canonical_orig_name(idx, id)
                }
            } else if *folded {
                idx.lower_name(id)
            } else {
                match exact_hay(idx, t, id) {
                    Some(h) => h,
                    None => return false,
                }
            };
            finder.find(hay).is_some()
        }
        Matcher::NamePrefix {
            bytes,
            folded,
            canonical,
        } => {
            let hay = if *canonical {
                if *folded {
                    ctx.canonical_lower_name(idx, id)
                } else {
                    ctx.canonical_orig_name(idx, id)
                }
            } else if *folded {
                idx.lower_name(id)
            } else {
                match exact_hay(idx, t, id) {
                    Some(h) => h,
                    None => return false,
                }
            };
            hay.starts_with(bytes)
        }
        Matcher::NameSuffix {
            bytes,
            folded,
            canonical,
        } => {
            let hay = if *canonical {
                if *folded {
                    ctx.canonical_lower_name(idx, id)
                } else {
                    ctx.canonical_orig_name(idx, id)
                }
            } else if *folded {
                idx.lower_name(id)
            } else {
                match exact_hay(idx, t, id) {
                    Some(h) => h,
                    None => return false,
                }
            };
            hay.ends_with(bytes)
        }
        Matcher::NameRegex { re, canonical } => {
            let hay = if *canonical {
                ctx.canonical_orig_name(idx, id)
            } else {
                idx.name(id)
            };
            re.is_match(hay)
        }
        Matcher::PathSub {
            finder,
            folded,
            canonical,
        } => {
            let hay = if *canonical {
                if *folded {
                    ctx.canonical_lower_path(idx, memo, id)
                } else {
                    ctx.canonical_orig_path(idx, memo, id)
                }
            } else if *folded {
                ctx.lower_path(idx, memo, id)
            } else {
                ctx.orig_path(idx, memo, id)
            };
            finder.find(hay).is_some()
        }
        Matcher::PathRegex { re, canonical } => {
            let hay = if *canonical {
                ctx.canonical_orig_path(idx, memo, id)
            } else {
                ctx.orig_path(idx, memo, id)
            };
            re.is_match(hay)
        }
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
