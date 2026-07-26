# ADR-0013: Measurement discipline (cold machine, back-to-back, real-volume absolute gate)

Date: 2026-06-11 / Status: Accepted

## Decision

Performance judgments are fixed as follows: (1) baseline recording and
`perf-gate`/`bench-check` only on a cold, idle machine; xtask compiles before
measurement, refuses the run unless both the preflight and postflight have mean
`% Processor Performance` >=95% and mean CPU time <=20%, and continuously
monitors the same clock counter while the benchmark runs (2) criterion
comparisons are limited to back-to-back A/B within the same session and run in
a fresh `CRITERION_HOME` seeded only from the baseline (3) the micro gate
requires the exact 28-report suite and reports a >10% regression only when the
median 95% confidence-interval lower bound exceeds +10%; the two explicitly
informational cases are present but ungated (4) the final judgment is the
real-volume absolute gate (initial index <=8s at 250k / <=60s at 1M, ready
working set <=110 B/entry, query p99 <=50ms, restore p50 <=1s) plus a query p50
relative +50%. The name distribution of the synthetic 1M benchmark is
calibrated to measured real C: data (identical fold 73.2% / unique names 53.2% /
mean WTF-8 length 29.7B), and `build_synthetic` asserts those ratios every run.

## Rationale

- This machine throttles to ~75% clock after a few minutes of all-core load, drifting p50 uniformly +30 to +46% (including snapshot restore, which is pure fixed-CPU work). Confirmed via simultaneous old/new A/B that "both equally slow = machine drift".
- criterion is also state-dependent: measuring the same code 40 minutes apart drifts +30% (parse_compile, a µs-class pure-CPU bench).
- p99-of-50-runs is effectively max (a single OS hiccup trips it). Even at 200 runs it swings +-60% -> p99 is gated only by the absolute budget (50ms).
- Synthetic criterion benches move +-12 to 23% from code layout alone (a synthetic "regression" that did not reproduce on real C: and was actually -4%). Real breakage shows up at +48% / 5x class, clearly outside the p50 relative +50% gate.
- The pre-calibration synthetic index had all-unique, lowercase-only names, making it useless for judging pool/column layout.

## Consequences

- p50 regressions under +50% are not detected by the real-volume gate
  (detection is handled by the back-to-back micro gate when the median 95% CI
  lower bound exceeds +10%).
- "all items including restore degrade uniformly" is treated as a thermal signature, not judged a code regression (re-measure cold).
- The baseline is machine-dependent. A missing/dirty identity or a Cargo.lock,
  rustc, processor, logical-CPU, or volume-entry drift beyond 10% fails closed;
  re-record deliberately on the measurement host.
- Baseline/check recipes are single xtask transactions rather than shared just
  dependencies. Each compiles first, checks immediately before and after its
  own measurement, and monitors the full run, so a real-volume run cannot heat
  the machine and silently invalidate the following micro run.
- Criterion checks use a newly cleared run directory and require the complete
  expected report set; same-ID files from an older run cannot be consumed.
- Baseline recording writes a candidate first. The candidate includes commit,
  dirty-content fingerprint, Cargo.lock, rustc, processor, timestamps, and
  counter summaries, and is promoted only after postflight succeeds. A failed
  or thermally invalid run cannot overwrite the previous baseline.
- Stable publishing requires a successful `performance-gate` workflow for the
  already-created immutable release tag and an unexpired evidence artifact.
  Only a separate hosted `workflow_run` job can convert that completed result
  into a release dispatch; the measurement runner has no publish authority.

## Re-examination triggers

- If a thermally stable machine dedicated to measurement (constant clock >=95%) becomes available, reconsider tightening the relative gate.
