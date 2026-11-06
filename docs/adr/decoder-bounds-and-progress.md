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
After excluding an immediate terminator, the remaining slice must fit a resident attribute header and a complete terminator header.
The attribute's bounded successor slice must keep that header before ID membership or value validation runs.
Reject truncated suffixes before decoding fields that can't belong to a complete chain.
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
The byte harnesses select `cadical` from the pinned Kani distribution or the pinned external Bitwuzla solver.
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
The admission harness compares the production validator with an independent bounded reference.
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
Attribute ID tracking preserves the low-ID bitset and keeps the first four distinct high IDs in an inline array.
Later high IDs spill into a vector without imposing a record or ID-count limit.
Duplicate rejection preserves every stored ID and leaves the state unchanged.
The inline prefix costs eight bytes of ID storage and avoids allocation for its four entries.
A production-bound harness compares each insertion with the original membership helper and independent sequence membership.
The harness quantifies every sequence of six arbitrary 16-bit IDs.
The bound includes inline-prefix exhaustion, spill insertion, duplicate rejection, and reallocation in the original vector implementation.
Native regressions require allocation-free inline insertion and unchanged state on duplicate rejection across all storage regions.
Generated native sequences exercise longer ID histories through an independent ordered set.

The bounded admission reference keeps its geometry, range, marker, type, and termination checks independent of the production parser.
Its seen-ID array has enough slots for the complete proof byte bound divided by the smallest resident header.
The original vector membership helper remains intact for the separate refinement check.
The record harnesses keep the original byte bound, field domains, assertions, outcome covers, and unwind bounds.
The ID classifier still covers every pair of 16-bit IDs in four disjoint classes.
The three-header cases partition the position of the first terminator into the first, second, or third attribute, or its absence.
A scalar harness proves the disjoint classification over every triple of 32-bit type values.
Each constructor keeps the original prefix-ID class and full record extent.
The constructor writes the selected terminator and excludes earlier terminators.
The absent case excludes all three terminators.
The union preserves every original type value and remaining byte without assuming admission.
Each admission case calls the production validator and preserved reference and checks equal results.
The absent case also proves that both validators reject.
The invariant observer retains every original assertion and selects its accepted branch from the actual validator result.
The absent invariant case proves rejection before inspecting the rejecting branch and checks that the observer agrees with the actual result.
First-terminator cases require reachable acceptance and acceptance at a prefix-ID boundary.
Cases with the terminator in the second or third attribute keep accepted and rejected covers.
Absent-terminator cases cover rejection and a distinct-ID chain with three complete minimal resident headers.
The combined covers keep both original outcomes without requiring an impossible outcome in a partition.
Every case keeps the original unwind bound of four.
All record-chain cases use `cadical` from the Kani bundle.
The terminator constructor supplies each prefix ID as an independent arbitrary 16-bit scalar and writes its little-endian bytes into the original record.
The constructor assumes only the selected ID storage classes.
Every original ID value, duplicate pair, and remaining byte stays in the combined input domain.
A separate harness writes each original ID back into an arbitrary record and checks equality at every arbitrary observation position.
The round-trip obligation requires scalar construction to reproduce every original byte array.
The byte bound, unwind bounds, assertions, and outcome covers remain unchanged.

The [scalar-ID attempt](https://github.com/P4suta/find-my-files/actions/runs/42148602868) leaves high-prefix-ID obligations incomplete after solver memory exhaustion and a safety deadline.
Changing array representation and [32-object-bit](https://github.com/P4suta/find-my-files/actions/runs/42161020140) or [48-object-bit](https://github.com/P4suta/find-my-files/actions/runs/42164231505) pointer encoding doesn't complete the affected obligations.
An [uninterpreted-function array attempt](https://github.com/P4suta/find-my-files/actions/runs/42154422423) also reports assertion failures in unchanged contract round trips.
The candidate keeps Kani's default pointer and array encodings.
Those reports establish unsuccessful verification attempts without establishing a production regression or a specific solver defect.
After inline ID storage removes allocation from bounded record chains, the [first attempt](https://github.com/P4suta/find-my-files/actions/runs/42169162607) still leaves one absent-terminator invariant unfinished at the existing 300-second SMT deadline.
The four absent-terminator obligations use the same bundled SAT solver as the remaining record-chain obligations.
Their input domains, assertions, covers, and unwind bounds stay unchanged.
The [SAT attempt](https://github.com/P4suta/find-my-files/actions/runs/42172091984) reaches later complementary third-length cases but leaves them incomplete after solver memory exhaustion.
Early successor validation removes invalid suffix work while preserving the independently checked admission domain.
The [successor-validation attempt](https://github.com/P4suta/find-my-files/actions/runs/42178449410) still leaves complementary third-length obligations incomplete after solver memory exhaustion.
Prefix-ID classification checks run before shared admission and invariant calls.
Each admission check calls the production validator and independent reference once after classification, rather than repeating their bodies in every classification branch.
The classification assertions, byte domains, properties, covers, and unwind bounds stay unchanged.
The external Linux solver has an exact version pin, archive byte count, and SHA-256 checksum.
The proof setup installs the verified executable alongside Kani.
The proof runner checks both executable versions and includes the tool configuration in its source fingerprint.
The existing per-process memory limit and safety deadlines still apply.
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
The record-chain harnesses also compare admission with the [bounded reference](../../engine/crates/fmf-core/src/ondisk/record_admission_reference.rs).
The reference retains independent header and range checks from [the source before the refactor](https://github.com/P4suta/find-my-files/blob/5f2853732cc179e5e25b165acfc49f3efb414342/engine/crates/fmf-core/src/ondisk/record.rs).
Its original membership helper participates in the insertion refinement proof.
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
