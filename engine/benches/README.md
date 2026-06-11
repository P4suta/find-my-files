# Real-volume benchmark baseline

`baseline.json` is the committed reference for `just bench-check`. Gates:
query p50 at >+50% vs baseline (smoke alarm — precision lives in
`just bench-micro-check`), query p99 ≤50ms and snapshot-restore p50 ≤1s as
absolute budgets (CLAUDE.md). Machine-dependent — regenerate with
`just bench-baseline C:` from an elevated terminal and record the
environment here:

| Recorded | CPU | Volume | Entries |
|---|---|---|---|
| 2026-06-11 | AMD Ryzen (Zen 3, Family 25 Model 80) | C: (NTFS) | 1,268,461 |

The synthetic micro-bench index is calibrated to measured real-C: name
statistics and `build_synthetic` asserts the distribution — re-calibrate
against a fresh `fmf stats C: --name-stats` dump instead of widening the
ranges (ADR-0013).

Report fields beyond the per-query timings:

- `cold_us` — first iteration (cold derived caches). Recorded, never gated.
- `restore` — snapshot save + 10 warm `load_from` runs (page-cache warm by
  design; cold I/O is not gateable without admin cache purging).
- The check warns when the volume's entry count drifted >10% from the
  baseline — re-record instead of chasing ghosts.

Micro-benchmarks (`cargo bench -p fmf-core`, no elevation) use a machine-
local criterion baseline: `just bench-micro-baseline` at session start,
`just bench-micro-check` per change, strictly back-to-back at one thermal
state. Discipline and the numbers behind it: ADR-0013 — baselines and
`perf-gate` only on a cool machine (verify ≥95% clock with
`typeperf "\Processor Information(_Total)\% Processor Performance" -sc 3`);
uniform regression across all queries including restore is the thermal
signature, not a code regression.
