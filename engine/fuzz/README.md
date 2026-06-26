# Fuzzing (cargo-fuzz / libFuzzer)

Coverage-guided fuzzing of the **untrusted-input decoders** that sit on a
privilege boundary: the named-pipe wire protocol the elevated `fmf-service`
parses from non-elevated clients. Run with `just fuzz <target> <secs>` (needs a
nightly toolchain on Linux/WSL); `just fuzz-build` just checks the harness
compiles. CI runs both targets for 60 s on every relevant push (`fuzz.yml`) and
on a weekly schedule.

## Targets

| Target | Surface | Why it matters |
|---|---|---|
| `frame_decode` | `fmf_proto::frame::read_frame` (16-byte header + length-prefixed body) | A lying length must not over-read or allocate unbounded. |
| `message_decode` | every `fmf_proto::messages` payload decoder (`decode_page`, `decode_event`, `decode_query_req`, the JSON cold paths, …) | Header fields drive slicing/UTF-8 over attacker-controlled bytes. |

These live in **fmf-proto**, a dependency-light leaf crate that builds on Linux,
so libFuzzer's instrumentation and sanitizers apply cleanly.

## Why fmf-core's parsers are not (yet) libFuzzer targets

The other untrusted-input decoders — the snapshot reader (`index::snapshot`,
which does `unsafe` POD reads driven by a length prefix), the USN record parser
(`usn::records`), the $MFT record parser (`scan::parse`), and the WTF-8 codec —
all live in **fmf-core**, which depends unconditionally on `ntfs-reader` and
`windows-sys`. That crate does not build for `x86_64-unknown-linux-gnu`, so it
cannot host a libFuzzer target here without first extracting the
platform-independent parsing into its own crate (an architectural change, out of
scope for the test-hardening work that added this note).

Until then those paths are guarded in-tree, on Windows, by **property tests**
that fuzz the same surfaces with `proptest` (panic-free / round-trip / structural
invariants):

- `index::snapshot::proptests::read_snapshot_survives_arbitrary_mutation_without_panicking`
  — arbitrary mutation/truncation of a valid snapshot, hitting the
  `try_reserve`/EOF guards before the checksum.
- `wtf8::proptests::*` — UTF-16 ↔ WTF-8 round-trips and fold-length invariants.
- `query::ast::proptests` / `query::compile::proptests` — parser/compiler never
  panic on arbitrary query text.
- `scan::parse::tests` / `tests/usn_replay.rs` — byte-fixture replay of the
  record decoders over hand-built and generated inputs.
