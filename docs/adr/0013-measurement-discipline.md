# ADR-0013: Measurement discipline (cold machine, back-to-back, real-volume absolute gate)

Date: 2026-06-11 / Status: Accepted

## Decision

Performance judgments are fixed as follows: (1) baseline recording and `perf-gate`/`bench-check` only on a cold, idle machine (confirm `% Processor Performance` >=95% beforehand with typeperf) (2) criterion comparisons limited to back-to-back A/B within the same session (3) the final judgment is the real-volume absolute gate (query p99 <=50ms, restore p50 <=1s) plus a query p50 relative +50%. The name distribution of the synthetic 1M benchmark is calibrated to measured real C: data (identical fold 73.2% / unique names 53.2% / mean WTF-8 length 29.7B), and `build_synthetic` asserts those ratios every run.

## Rationale

- This machine throttles to ~75% clock after a few minutes of all-core load, drifting p50 uniformly +30 to +46% (including snapshot restore, which is pure fixed-CPU work). Confirmed via simultaneous old/new A/B that "both equally slow = machine drift".
- criterion is also state-dependent: measuring the same code 40 minutes apart drifts +30% (parse_compile, a µs-class pure-CPU bench).
- p99-of-50-runs is effectively max (a single OS hiccup trips it). Even at 200 runs it swings +-60% -> p99 is gated only by the absolute budget (50ms).
- Synthetic criterion benches move +-12 to 23% from code layout alone (a synthetic "regression" that did not reproduce on real C: and was actually -4%). Real breakage shows up at +48% / 5x class, clearly outside the p50 relative +50% gate.
- The pre-calibration synthetic index had all-unique, lowercase-only names, making it useless for judging pool/column layout.

## Consequences

- p50 regressions under +50% are not detected by the real-volume gate (detection is handled by the back-to-back 10% median gate in `bench-micro-check`).
- "all items including restore degrade uniformly" is treated as a thermal signature, not judged a code regression (re-measure cold).
- The baseline is machine-dependent. Re-record when the volume's entry count drifts more than 10% from the baseline.

## Re-examination triggers

- If a thermally stable machine dedicated to measurement (constant clock >=95%) becomes available, reconsider tightening the relative gate.
