# Real-volume benchmark baseline

`baseline.json` is the committed reference for `just bench-check`. The gate
is a smoke alarm, not a precision instrument (that is
`just bench-micro-check`): query p50 fails at >+50% vs baseline (the
machine's thermal envelope alone moves wall clock ±30%), while query p99
(≤50ms) and snapshot-restore p50 (≤1s) fail against the absolute
acceptance budgets from CLAUDE.md. Machine-dependent — regenerate with
`just bench-baseline C:` from an elevated terminal and record the
environment here:

| Recorded | CPU | Volume | Entries |
|---|---|---|---|
| 2026-06-11 (am) | AMD Ryzen (Zen 3, Family 25 Model 80) | C: (NTFS) | 1,275,274 |
| 2026-06-11 (pm, adds `Win`) | AMD Ryzen (Zen 3, Family 25 Model 80) | C: (NTFS) | 1,268,461 |

The synthetic index behind the micro-benchmarks is calibrated to the
real-C: name statistics (`fmf stats C: --name-stats`, 2026-06-11:
fold-identical 73.2%, unique names 53.2%, mean WTF-8 length 29.7B;
docs/RESEARCH.md has the full numbers). `build_synthetic` asserts those
ratios on every run — re-calibrate the generator against a fresh
`--name-stats` dump instead of widening the ranges.

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

**Thermal discipline** (measured 2026-06-11): after minutes of all-core
load (builds, criterion runs) this machine throttles to ~75% clock and
wall-clock numbers degrade 20%+ progressively — an A/B of old vs new code
showed both equally slow, i.e. pure machine drift. Record baselines and
run `bench-check`/`perf-gate` only on a cool, idle machine; verify with
`typeperf "\Processor Information(_Total)\% Processor Performance" -sc 3`
(expect ≥95%). A gate failure where *everything* regressed uniformly —
including snapshot restore, which is pure fixed CPU work — is the thermal
signature, not a code regression.

The same applies to the criterion baseline: it is only comparable within
one session at one thermal state (identical code measured 40 minutes
apart drifted +30% on a µs-scale pure-CPU bench). The intended loop is
exactly `bench-micro-baseline` → change → `bench-micro-check`,
back-to-back — never compare across hours.
