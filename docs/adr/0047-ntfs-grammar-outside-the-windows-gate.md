# ADR-0047: The NTFS byte grammar lives outside the Windows gate

Date: 2026-07-27 / Status: Accepted

## Context

`fmf-core` splits into `#[cfg(windows)]` modules and pure ones, and `lib.rs`
has always justified that split by fuzz reachability: the pure parsers compile
on Linux, which is what lets `engine/fuzz` drive them under libFuzzer with the
address sanitizer (ADR-0022's property tests plus the coverage-guided pass).

That justification did not survive contact with the module list. The largest
and most exposed parser surface in the product sat *inside* the gate:

| module | what it decodes |
|---|---|
| `scan/ntfs.rs` | boot sector, `FILE` record header, attribute headers, `$FILE_NAME` |
| `scan/attribute_list.rs` | `$ATTRIBUTE_LIST` entries, non-resident run maps, extent closure |
| `scan/record.rs` | whole-attribute-chain validation of one fixed-up record |
| `scan/volume_io.rs::apply_fixup` | the update-sequence array |

Together roughly 2,700 lines of decoder plus its tests — the bulk of the
untrusted-byte surface in the engine.

None of these had any Windows dependency — `ntfs.rs` imported only
`thiserror`, `record.rs` imported nothing at all, and `attribute_list.rs` was
already generic over `impl Read + Seek`. There was no `cfg(windows)` anywhere
inside `scan/`; the gate existed solely at the `pub mod scan;` declaration in
`lib.rs`, and these files were gated by accident of where they were filed.

The cost was not theoretical. `.github/workflows/ci.yml` lists
`engine/crates/fmf-core/**` in its `fuzz` path filter and the `contract-lint`
job claimed contract/proto were "the only engine crates that compile
off-Windows". Together those asserted a coverage story the layout did not
deliver: a change to the NTFS grammar re-ran the fuzz job, and the fuzz job
could not see the NTFS grammar.

This is the highest-value attack surface in the product. A crafted VHDX or a
hostile USB stick lets an attacker choose every byte these decoders read, and
`fmf-service` parses them as `LocalSystem`. `#![forbid(unsafe_code)]` means the
language rules out memory unsafety, so the residual hazard is precisely what
coverage-guided fuzzing is good at finding and property tests are not: an
out-of-bounds slice or an arithmetic overflow that panics, which is a denial of
service against a `LocalSystem` service.

## Decision

Move the four pure surfaces above into a new ungated `fmf_core::ondisk`
module — `ondisk::ntfs`, `ondisk::record`, `ondisk::attribute_list`,
`ondisk::fixup` — and make them `pub` so a separate fuzz crate can reach them.

**The `#[cfg(windows)]` boundary is drawn at acquisition, not at subject
matter.** A module is gated when it opens a handle or issues an FSCTL
(`scan/volume_io.rs`, `usn/session.rs`, `mft.rs`, `engine/`), not because it is
"about NTFS". `scan/` keeps acquisition and orchestration; `ondisk/` owns the
grammar those modules feed.

No decoder logic changed. The move is file relocation, visibility, and the doc
comments that `missing_docs` (deny) requires on a newly public surface. Every
existing test — including the proptest no-panic sweeps — moved with its module
and still runs unchanged, and `apply_fixup`'s tests moved out of
`volume_io.rs`'s Windows-only test module so they run on any host.

### This does not touch ADR-0018's two-seam limit

[ADR-0018](0018-contract-single-source.md) caps the engine at two trait seams
(`SnapshotStore` / `JournalSource` in `engine/seams.rs`) and forbids further
port-ification. That limit is about **trait indirection**: each new port adds a
dynamic-dispatch boundary, a set of test doubles, and a place where production
and test behaviour can silently diverge.

This ADR adds no trait, no generic parameter, no injection point, and no test
double. It moves concrete functions between files and changes which `mod`
statement they hang from. The call graph after the move is identical to the
call graph before it — `scan::parse` still calls `attributes_complete`
directly, monomorphically, with no seam in between. The seam budget is
unaffected and remains at two.

### `scan/parse.rs` is deliberately not included

`parse.rs` is a parser too, and it is the natural next candidate, but it is
excluded because it is not pure:

- `use crate::mft::collect_searchable_names` — `mft` is `#[cfg(windows)]`
- `use crate::index::{EncodedEntry, Frn, VolumeIndexBuilder}` — it does not
  merely decode bytes, it appends rows to the index under construction

Dragging it across would mean either ungating `mft` or pulling index
construction into the grammar module, both of which would make `ondisk` mean
something looser than "decodes untrusted bytes, touches nothing". The bytes
`parse.rs` decodes are already reachable through `ondisk::ntfs` and
`ondisk::record`, which is where the byte-level attack surface actually lives;
what `parse.rs` adds on top is name-selection policy and builder calls.

## Consequences

- `fmf-core`'s public API grows by the whole `ondisk` tree. This is the point —
  a fuzz target in a separate crate cannot reach `pub(crate)` — but it does mean
  the NTFS grammar is now a documented, semver-relevant surface rather than an
  internal detail. All of it is now documented, which it was not before.
- `ondisk::fixup::fixup_layout` stays `pub(crate)`: it is `apply_fixup`'s helper
  and is exercised through it, so it is not part of the surface being widened.
- The fuzz targets themselves are **not** added by this ADR. Reachability is a
  prerequisite, not the coverage; until the targets land, the honest statement
  is that `ondisk` *can* be fuzzed, and `fuzz.yml`'s surface list is left
  describing only what actually runs.
- `ci.yml`'s `contract-lint` comment is corrected. `fmf-core` is still not
  linted on Linux, now for a stated reason: the Linux view of it is a strict
  subset of the Windows clippy run, so it would duplicate an existing gate.
- The Linux build of `ondisk` is verified by CI (the `fuzz` job builds
  `fmf-core` on `ubuntu-24.04`), not locally: this machine's toolchain is
  mise-pinned to the MSVC target and adding a rustup target ad hoc is against
  the machine's tooling rules.

## Re-examination triggers

- **A `cfg(windows)` becomes necessary inside `ondisk/`.** That would mean the
  acquisition/grammar line was drawn in the wrong place, and the module should
  be re-split rather than gated.
- **`mft.rs`'s Windows dependency is isolated.** `mft` is gated only for
  `peak_working_set` / `current_working_set` / `current_private_bytes`
  (`windows-sys` `ProcessStatus`); its name-selection policy
  (`collect_searchable_names`, `is_searchable_namespace`) is pure. If those
  process-memory helpers move out, `parse.rs`'s blocker reduces to
  `VolumeIndexBuilder` alone and the exclusion above should be revisited.
- **A fuzz target finds a defect in a module that was moved here.** That
  retroactively prices the move and argues for extending the same treatment to
  the next-most-exposed decoder rather than stopping at this boundary.
- **The public surface becomes a maintenance burden** — e.g. an external
  consumer starts depending on `ondisk` types, or doc churn on the grammar
  becomes a routine cost. Then the alternative is a `#[doc(hidden)]` or
  fuzz-only feature gate, accepting that it weakens the "it is documented"
  benefit above.
