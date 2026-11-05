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
An independent header partition rejects every inconsistent allocation length or used length beyond the record.
It quantifies the original arbitrary header bytes and length domain.
Each fixed-offset partition constructs its offset and allocation bytes directly.
The construction also applies to each terminal offset.
Its arbitrary one-byte used length covers every admitted value within the unchanged byte bound.
All remaining bytes stay arbitrary.
The rejected-header and fixed-offset partitions together cover the complete original input domain.
Each offset has separate admission-equivalence and accepted-chain invariant harnesses.
The two families construct a first residency marker of zero or one.
Each offset has an independent invalid-marker harness for every other marker value.
The invalid-marker constructor fixes the offset and allocation fields just as the resident constructor does.
The inconsistent-header harness covers the rejected headers outside that construction.
Acceptance with an invalid first marker requires an immediate terminator and the same checked record geometry.
The three marker partitions together cover the same complete byte domain for each offset.
The admission harness compares the production validator with the preserved reference.
Resident admission uses partitions for the first attribute length.
Each aligned length that can leave room for a terminator has a constant case.
A complementary case keeps every other 32-bit length.
Every case includes immediate terminators and malformed chains.
The union preserves every original field value without assuming either validator's result.
The earliest offsets can contain three resident attributes.
An arbitrary second length gives later header fields symbolic addresses.
The affected cases at the earliest offsets also partition the second length into constant aligned lengths and their complete 32-bit complement.
The second header keeps arbitrary values for its type, ID, name metadata, and value metadata.
The construction includes absent, malformed, and valid second attributes.
When two attributes leave room for a third header, the affected cases also partition the third length.
Constant cases include every aligned resident length that fits the remaining record, even a length that leaves no terminator.
A complementary case keeps every other 32-bit length.
The third header keeps arbitrary type, ID, name, and value fields.
The construction includes absent, malformed, and valid third attributes.
The same geometries partition the second and third residency markers into three disjoint classes.
The first class keeps every nonzero second marker and every third marker and length.
The second class fixes the second marker to zero and keeps every nonzero third marker and every third length.
The third class fixes both markers to zero and uses the complete third-length partitions.
The combined classes preserve every original marker pair without assuming admission or complete headers.
Separate admission and invariant harnesses cover both complementary marker classes.
The actual classifier has a scalar harness for every pair of byte values.
When the third attribute fills the proof buffer, two more extent partitions cover every record length and used-byte count.
The full-extent constructor fixes both fields and the input slice to the complete proof buffer.
The complementary constructor keeps every other pair without changing the byte domain.
The complementary cases permit at most two advances and the final probe.
The full-extent cases still include the third advance and fourth probe.
An insufficient bound must still fail an unwinding assertion.
The invariant obligations use the same length partitions as admission.
A dedicated harness checks the complement classifier for every 32-bit length across the supported aligned byte capacities.
Another harness checks the final-header complement across every 32-bit length and its supported capacities.
The invariant harness calls the production validator and independently checks every accepted attribute.
Requiring both harnesses preserves the conjunction of the former assertions without three traversals in one solver problem.
The invariant harness also bounds the attribute count by the smallest resident header and the space before the terminator.
The earliest resident case covers an accepted chain with more than one attribute.
Its most detailed invariant case also requires a reachable accepted chain with three attributes.
The earliest non-resident case covers an accepted non-resident attribute.
Every other input byte stays arbitrary under the existing constructive header fields.
The byte and length bounds remain unchanged.
The chain loops return from inside their bodies.
Their unwind bounds count every possible attribute advance and the final returning probe.
The general offset bound divides the remaining proof bytes by the smallest resident header and adds one.
A constant first length leaves only the remaining bytes for later attributes.
The complementary length partition permits at most one advance before rejecting a missing terminator.
The first-length cases use their corresponding bounds without removing any bytes or field values.
The earliest terminal-only offset includes one possible resident attribute followed by rejection.
The later terminal offsets reject every nonterminal attribute before advancing.
The other rejected offsets permit one empty-region probe and no attribute advance.
Exhaustive constant patterns classify the terminal offsets without a separate slice traversal.
Compile-time checks bind the bounds to the proof geometry.
After a first non-resident header, the remaining proof bytes can't contain another complete attribute.
The bounds for a non-resident first attribute include the attribute and final probe.
The [unwinding assertions](https://model-checking.github.io/kani/tutorial-loop-unwinding.html) remain enabled and make an insufficient bound fail verification.
Each proof partition verifies one harness at a time to limit concurrent resource demand.
The [partition collector](sharded-proof-evidence.md) requires the complete inventory across the bounded jobs.
The proof watchdog and hosted job keep their declared safety deadlines.
A deadline stop leaves the proof incomplete.
Every registered harness must pass before release.
The record-chain harnesses also compare admission with a [preserved reference](../../engine/crates/fmf-core/src/ondisk/record_admission_reference.rs).
The reference retains the parser from [the source before the refactor](https://github.com/P4suta/find-my-files/blob/5f2853732cc179e5e25b165acfc49f3efb414342/engine/crates/fmf-core/src/ondisk/record.rs).
The file-header harness compares admission with the original geometry and reference checks from [the same source](https://github.com/P4suta/find-my-files/blob/5f2853732cc179e5e25b165acfc49f3efb414342/engine/crates/fmf-core/src/ondisk/ntfs.rs).
The byte adapter harness links each update-sequence field to its independent little-endian bytes.
Successful proof results establish those properties under Kani's Rust and standard-library semantics.
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
