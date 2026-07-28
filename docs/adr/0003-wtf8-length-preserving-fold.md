# ADR-0003: Lossless WTF-8 storage, length-preserving fold, canonical search

Date: 2026-06-11 (amended 2026-07-26) / Status: Accepted

## Decision

Stored names remain byte-exact WTF-8. The folded dictionary still applies only
single-code-point lowercase mappings of identical encoded length; this preserves
ADR-0004's shared name-length layout and snapshot format.

Ordinary non-ASCII name/path literals and globs compare NFC query-time views.
Valid scalar spans on each side of a lone surrogate normalize independently;
the surrogate's WTF-8 bytes are an opaque barrier. Explicit regex mode continues
to evaluate the original spelling, because normalization would change regex
offset and syntax semantics.

Candidate generation unions the existing raw folded-pool sweep with a
non-ASCII dictionary completion pass. Because NFC and the length-preserving
storage fold do not commute for every spelling, fold-nonidentical entries also
receive an original-spelling completion pass; the source matcher is then
verified as a residual from the original spelling. Normalization-inert ASCII
queries take the original sweep and matcher path unchanged. ASCII `;`, `` ` ``,
and `K` (plus folded `k`) opt into completion because the locked Unicode table
has canonical singleton aliases for them.
Canonical scratch is per-query/thread and retained only for reuse during that
query; no normalized pool or snapshot column is added.

## Consequences

- NFC, NFD, and mixed canonical spellings match for substring, prefix, suffix,
  extension, path, and whole-name glob searches.
- Ill-formed UTF-16 still round-trips exactly through stored WTF-8 and FFI.
- Steady-state index RAM and the normalization-inert ASCII hot path are
  unchanged. Canonical queries pay a bounded dictionary scan plus an
  original-spelling pass over fold-nonidentical non-ASCII entries.
- Multi-character lowercase expansion remains outside the storage fold.
