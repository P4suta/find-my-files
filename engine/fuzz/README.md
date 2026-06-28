# Fuzzing (cargo-fuzz / libFuzzer)

Coverage-guided fuzzing of the **untrusted-input decoders** on a privilege
boundary: the named-pipe wire protocol the elevated `fmf-service` parses from
non-elevated clients, plus the fmf-core parsers that ingest on-disk / OS-supplied
bytes. Run with `just fuzz <target> <secs>` (needs a nightly toolchain on
Linux/WSL); `just fuzz-build` just checks the harnesses compile. CI runs every
target for 60 s on each relevant push (`fuzz.yml`) and on a weekly schedule.

## Targets

| Target | Surface | Why it matters |
|---|---|---|
| `frame_decode` | `fmf_proto::frame::read_frame` (16-byte header + length-prefixed body) | A lying length must not over-read or allocate unbounded. |
| `message_decode` | every `fmf_proto::messages` payload decoder (`decode_page`, `decode_event`, `decode_query_req`, the JSON cold paths, …) | Header fields drive slicing/UTF-8 over attacker-controlled bytes. |
| `query_parse` | `query::parse` → `compile`, plus `compile_whole_regex` | Query text crosses the privilege boundary; the regex builder must hit its size/DFA caps, not panic or run away. |
| `index_snapshot` | `index::VolumeIndex::read_snapshot` | `unsafe` POD reads (`set_len`) sized by an untrusted length prefix — a corrupt `.fmfidx` must `Err`, not over-read or over-allocate. |
| `usn_records` | `usn::parse_buffer` (+ `encode_buffer`) | Attacker-influenceable `RecordLength`/name offsets; the walk must terminate and never slice out of bounds. |
| `wtf8_decode` | `wtf8::wtf8_to_utf16` + fold helpers | Ill-formed UTF-16 / unpaired surrogates must not panic or read out of bounds. |

`fmf-proto` is a dependency-light leaf crate that builds on Linux. The fmf-core
targets build there too because the crate's Windows deps (`ntfs-reader` /
`windows-sys`) are `cfg(windows)`-gated (the `mft` / `scan` / `engine` modules
are `#[cfg(windows)]`), leaving the pure parsers to compile for Linux — so
libFuzzer's instrumentation and ASan apply cleanly. This is not cross-platform
support (the app stays Windows-only); only the OS-independent parsers compile
off-Windows.

## Relationship to the property tests

Every fuzzed surface keeps its in-tree `proptest` coverage on Windows (panic-free
/ round-trip / structural invariants) — libFuzzer adds coverage-guided
exploration with sanitizers over the same code:

- `index::snapshot::proptests::read_snapshot_survives_arbitrary_mutation_without_panicking`
  — arbitrary mutation/truncation of a valid snapshot, hitting the
  `try_reserve`/EOF guards before the checksum.
- `wtf8::proptests::*` — UTF-16 ↔ WTF-8 round-trips and fold-length invariants.
- `query::ast::proptests` / `query::compile::proptests` — parser/compiler never
  panic on arbitrary query text.
- `scan::parse::tests` / `tests/usn_replay.rs` — byte-fixture replay of the
  record decoders over hand-built and generated inputs.
