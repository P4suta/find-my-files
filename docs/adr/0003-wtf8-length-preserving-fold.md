# ADR-0003: WTF-8 storage and length-preserving fold

Date: 2026-06-11 / Status: Accepted

## Decision

Names are stored as WTF-8. The search fold applies only "single-character lowercasing that keeps the same encoded length" (wtf8.rs). No NFC/NFD normalization.

## Rationale

- NTFS names are raw UTF-16 sequences and may contain ill-formed (unpaired) surrogates. WTF-8 preserves these losslessly (FFI contract: strings are UTF-8, filenames are WTF-8; the C# side restores UTF-16 via a dedicated decode)
- Because the fold is length-preserving, name_off / name_len can be shared between the folded pool view and the original-text view (the precondition for the fold-overflow layout = ADR-0004)
- A match position in the folded pool maps to the same byte position in the original text (anchor-position preservation). Residual verification holds without offset conversion

## Consequences

- Lowercasing that expands to multiple characters and normalization equivalence (NFC/NFD) are not absorbed by search (known limitation)
- The fold rule is shared between the engine core and the `fmf stats --name-stats` measurement (no rule mismatch between statistics and implementation)

## Re-examination triggers

- If real-world harm from NFC/NFD mismatch is reported continuously (even then storage stays WTF-8; only the fold layer is redesigned)
