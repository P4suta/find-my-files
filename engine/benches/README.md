# Real-volume benchmark baseline

`baseline.json` is the committed reference for `just bench-check`(20% regression
gate). Machine-dependent — regenerate with `just bench-baseline C:` from an
elevated terminal and record the environment here:

| Recorded | CPU | Volume | Entries |
|---|---|---|---|
| (未記録 — 昇格シェルで `just bench-baseline C:` を実行して追記) | | | |

Report fields beyond the per-query timings:

- `cold_us` — first iteration of each query (cold derived caches). Recorded
  for analysis, never gated (single sample).
- `restore` — snapshot save + 10 warm `load_from` runs (the CPU-bound share
  of the restore→ready ≤2s gate; page-cache warm by design — cold I/O needs
  admin cache purging and is too noisy to gate). p50 is gated at 20% with a
  50ms noise floor.
- The check also warns when the volume's entry count drifted >10% from the
  baseline — regression verdicts are unreliable then; re-record.

Micro-benchmarks (`cargo bench -p fmf-core`, no elevation) carry their own
local baseline: `just bench-micro-baseline` at the start of an optimization
session, `just bench-micro-check` (criterion + 10% median gate) per change.
That baseline lives in `target/criterion` — machine-local, not committed.
