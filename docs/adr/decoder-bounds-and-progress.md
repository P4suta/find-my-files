---
date: 2026-11-05
---

# Decoder bounds and progress

## Status

Proposed.
This refines the parser boundary in [Pure core and shared runtime](pure-core-and-shared-runtime.md).

## Context

Byte decoders repeat some bounds checks and manually advance offsets through validated input.
Equivalent guards obscure the accepted domain, while a non-progressing offset mutation can leave a test inconclusive.
The two encoder functions contain duplicate arithmetic for surrogate pairs.
Preserve the accepted input domain and decoded values.
Keep caller output unchanged when a conversion fails.

## Decision

Complete record-chain validation consumes the checked attribute decoder's ranges and header fields.
The chain validator still owns ordering, unique IDs, alignment, region ordering, and complete termination.
A private name-value decoder validates the bounded resident slice without repeating attribute geometry.
The attribute adapter preserves the checked resident range and returns the name-value decoder's result.
Attribute traversal consumes the remaining slice after each decoded attribute.
The checked parser establishes a positive attribute length before traversal advances.
Record headers share the checked update-sequence geometry validator.
Fixed headers consume a checked byte prefix.
Boot cluster counts and record-size codes have nonzero representations.

The point decoder returns a nonzero consumed width.
Validation and output conversion advance by that width through a remaining input slice.
An accepted step reduces the remaining byte count.
The initial validation pass still protects caller output from partial writes on rejection.
Both UTF-16 frontends share one checked surrogate-pair conversion.
The point decoder adds each continuation digit after shifting the previous digits.
The width harness independently reconstructs every accepted point with the original bitwise composition.

Reference composition places the record and sequence bytes in their separate fields.
The bounds already implied by checked input fields need no duplicate admission guard.
Production-bound proofs check the retained layout and range properties.
Native fixtures check exact endpoints, malformed and overlapping regions, forbidden name units, independent header bytes, and surrogate boundaries.

The proof inventory records the source, harness names, and unwind bounds.
Scalar reference and surrogate checks quantify their complete integer domains.
Byte-array checks state finite input bounds and require reachable accepted and rejected cases.
The output harnesses quantify every byte array at each stated length.
Output and filename checks choose one arbitrary observation position.
The observation index ranges over every original output cell or name unit.
The length variables for record and filename inputs use one byte with the original bounds.
A compile-time check rejects proof buffers larger than the length representation.
The byte harnesses select a solver from the pinned Kani distribution: `cadical`.
Kani supports [per-harness solver selection](https://model-checking.github.io/kani/reference/attributes.html#kanisolversolver) without changing the property or input domain.
The record-chain harnesses partition the complete attribute-offset domain without changing the byte bounds.
Each fixed-offset partition, including each terminal offset, constructs its two header bytes directly and keeps every other byte arbitrary.
Their unwind bounds follow the smallest resident header and the remaining proof bytes.
The bounds leave room for the terminal or rejected header after the largest admitted sequence.
The earliest terminal-only offset includes one possible resident attribute followed by rejection.
The later terminal offsets reject every nonterminal attribute before advancing.
The other rejected offsets permit one empty-region probe and no attribute advance.
Exhaustive constant patterns classify the terminal offsets without a separate slice traversal.
Compile-time checks bind the bounds to the proof geometry.
Unwinding assertions remain enabled.
The runner verifies one harness at a time to reduce concurrent resource demand.
The proof watchdog and hosted job use longer safety deadlines for the expanded inventory.
A deadline stop leaves the proof incomplete.
Every registered harness must pass before release.
The record-chain harnesses also compare admission with a [preserved reference](../../engine/crates/fmf-core/src/ondisk/record_admission_reference.rs).
The reference retains the parser from [the source before the refactor](https://github.com/P4suta/find-my-files/blob/5f2853732cc179e5e25b165acfc49f3efb414342/engine/crates/fmf-core/src/ondisk/record.rs).
The file-header harness compares admission with the original geometry and reference checks from [the same source](https://github.com/P4suta/find-my-files/blob/5f2853732cc179e5e25b165acfc49f3efb414342/engine/crates/fmf-core/src/ondisk/ntfs.rs).
The byte adapter harness links each update-sequence field to its independent little-endian bytes.
The proof results establish those properties under Kani's Rust and standard-library semantics.
They don't establish validity of every file system image or Windows I/O behavior.
Native volume tests and performance measurements remain required at those boundaries.

Keeping duplicate guards would leave two authorities for one accepted range.
Adding accepted mutation exceptions would leave the representation and missing boundary fixtures unchanged.
The chosen representation exposes progress and field ownership in production types and operations.

## Consequences

The pure core keeps safe Rust, its existing external interfaces, and no production dependencies.
The byte and persistence formats remain unchanged.
The engine still has its existing two resource seams.
Every new input step or layout rule needs the matching source-bound proof and native regression.
Reconsider the finite byte bounds when a changed format or traversal requires longer proof inputs.
