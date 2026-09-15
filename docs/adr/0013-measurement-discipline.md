# ADR-0013: Measurement discipline (cold machine, back-to-back, real-volume absolute gate)

Date: 2026-06-11 / Status: Accepted

## Decision

Performance judgments are fixed as follows: (1) baseline recording and
`perf-gate`/`bench-check` only on a cold, idle machine; xtask compiles before
measurement, refuses the run unless the preflight has mean
`% Processor Performance` >=95% and both the preflight and the postflight have
mean CPU time <=20%, and monitors the same clock counter for the whole measured
run — judging that run against the clock the baseline recorded while measuring
itself (<=5 points of drift, <=20 points of extra CPU) rather than against an
absolute bar (amended 2026-09-15, below) (2) criterion
comparisons are limited to back-to-back A/B within the same session and run in
a fresh `CRITERION_HOME` seeded only from the baseline (3) the micro gate
requires the exact 28-report suite and reports a >10% regression only when the
median 95% confidence-interval lower bound exceeds +10%; the two explicitly
informational cases are present but ungated (4) the final judgment is the
real-volume absolute gate plus a query p50 relative +50%. **The pass line is
recorded here and only here** — initial index <=8s at 250k / <=60s at 1M, ready
working set <=110 B/entry, query p99 <=50ms, restore p50 <=1s. These are
ceilings chosen as the point below which the product stops feeling instant, not
targets: the measured values sit far under them (real C: ~2s at 1.27M, p99
single-digit ms), and closing that margin is a regression even while the gate
still passes. Any other document quoting a figure is quoting this list. The name distribution of the synthetic 1M benchmark is
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
  dirty-content fingerprint, the semantic Cargo dependency graph (workspace-only
  version bumps are normalized; dependency/source/checksum drift is not), rustc,
  processor, timestamps, and
  counter summaries, and is promoted only after postflight succeeds. A failed
  or thermally invalid run cannot overwrite the previous baseline.
- **Amended 2026-09-15: the loaded run is judged for comparability, not for
  absolute clock.** The original wording was implemented as one 95% bar applied
  to the preflight, the postflight *and* the whole-run monitor. That is
  unsatisfiable by construction here: the engine scans and queries on every core
  by design, and the rationale above records this very machine falling to ~75%
  under exactly that load. The chronology shows it was never satisfied — the
  bar landed in `0f3d873` (2026-07-28) and the last baseline was recorded on
  2026-07-11, so no baseline has ever been recorded under it, and the real-volume
  half of `perf-gate` has been dead since. A measured run now records its clock
  summary into the measurement identity (it always did) and is compared against
  the summary the baseline recorded for its own run. The band is derived, not
  chosen: the rationale's 25-point drop costs at most +46% of time, so at most
  1.84% per point, and thermal drift must stay under the smallest regression
  these gates will report (+10%, decision item 3) — 10 / 1.84 = 5.4, taken as 5
  points. Extra CPU beyond the baseline's is allowed up to the same 20% this ADR
  already calls harmless foreign work. The postflight keeps the CPU half only:
  right after an all-core run the clock is legitimately still recovering (94.8%
  measured), and the *next* run's own cold preflight is what protects it from
  starting warm.
- **Amended 2026-09-15 (second): comparing against a stored Criterion baseline
  is not the comparison decision item (2) authorizes, so the micro suite is not
  part of `perf-gate`.** Measured on the reference machine at one commit with a
  clean tree, varying nothing but the time between recording the baseline and
  checking against it: adjacent, the whole 29-report suite agrees within a +0.3%
  median CI lower bound and the gate passes; ~25 minutes apart it fails at
  +13.1% (`post_usn/first_query_sorted_size`); ~45 minutes, +19.3%
  (`query/one_char`); ~65 minutes, +102.1% (`query/composite_path`, whose sample
  minimum alone moved +66%). A different report fails each time and the counters
  see none of it — clock 76.0-76.5%, CPU 66.6-67.5%, 26 GB of 40 GB free
  throughout. That is the +30%-over-40-minutes drift the rationale above already
  records, so the instrument is behaving as documented and the adjacent run is
  the positive control proving it sound. What is invalid is the comparison:
  item (2)'s back-to-back A/B means interleaving two *code versions* in one
  session so the drift lands on both arms, and recording then checking the same
  commit cannot detect anything at all. Until that A/B exists, `perf-gate` is
  the real-volume absolute gate item (4) already names as the final judgment,
  and the Criterion suite is run by hand as an informational instrument. No
  threshold was widened and no report was moved to the informational set.
- **Also measured 2026-09-15: the real-volume half perturbs the Criterion half
  that follows it.** Run immediately after the 3.5M-entry scan,
  `query/composite_path`'s median rose 29.7% while its sample minimum rose only
  6.4%, at the same clock and CPU as an undisturbed run. The bullet below
  claiming each transaction's own preflight and postflight stop a real-volume
  run from silently invalidating the following micro run is false as
  implemented: the perturbation is not clock-shaped, so no clock check can see
  it.
- **Retired by [ADR-0048](0048-direct-dispatch-release-workflow.md) (2026-07-28):
  the four CI measurement workflows below were deleted — their organization
  runner group cannot exist on a user-owned repository, so the chain never ran.
  The gate is now the manual `just perf-gate` on the reference machine, and the
  committed baseline is recorded with `just bench-baseline` and landed by PR.
  The measurement discipline itself — every threshold, preflight, and evidence
  rule above — is unchanged and still enforced by xtask. The remaining bullets
  in this section describe how CI *would* serialize the instrument, re-verify
  its evidence, and gate publication on it if an organization ever provides
  one.**
- Recording and gating are serialized on the same instrument by the
  default-branch `performance-controller` workflow and protected `performance`
  environment. The controller emits a run/attempt-only runner name and label;
  the external provisioner must roll the OS/workspace disk back before each job,
  obtain a JIT configuration, and launch the runner with `--ephemeral`. Jobs
  additionally require the static `fmf-jit-ephemeral` label and verify the exact
  Actions job label set plus `RUNNER_NAME` before checkout, so a standing runner
  is never queue-eligible.
- Criterion baselines live on a separately attached `P:` volume with a protected
  SYSTEM/Administrators DACL. The trusted pre-checkout step rejects reparse
  points and same-volume storage, then copies the tree to disposable scratch and
  verifies a path/length/SHA-256 manifest. Repository code never benchmarks
  directly against the persistent source.
- Each real and micro gate writes schema-1 deterministic evidence containing the
  target commit, semantic Cargo.lock identity, machine/counter identity, the
  complete expected case set, actual/baseline/delta/threshold/verdict, and
  `finite`/`passed`. A failed regression retains evidence but cannot authorize
  release. The hosted `performance-release` job downloads the exact run artifact,
  rejects every file outside the two-summary allowlist, independently recomputes
  Cargo.lock identity and every verdict, and accepts only two complete finite
  passing summaries.
- Only a read-only hosted job may validate a real-volume baseline candidate. It
  passes the immutable candidate SHA-256 to a separate environment-gated write
  job, which re-downloads the exact-run artifact and verifies that digest before
  minting the narrow PR token.
- Stable publishing requires a successful gate controller run for the
  already-created immutable release tag and an unexpired evidence artifact.
  Only a separate hosted `workflow_run` job can convert that completed result
  into a release dispatch; the measurement runner has no publish authority.

## Re-examination triggers

- If a thermally stable machine dedicated to measurement (constant clock >=95%) becomes available, reconsider tightening the relative gate.
- Restore the Criterion suite to `perf-gate` once it compares two code versions
  interleaved within one session — a baseline-commit worktree against HEAD —
  which is the comparison item (2) authorizes. The positive control is recorded
  above: adjacent comparisons agree within 0.3%, so the instrument is sound and
  only the comparison needs rebuilding.
- If a measured run stops reproducing the baseline's clock regime within the
  5-point band on the reference machine, re-derive the band from fresh
  measurements and record the numbers here. Widening it to make a run pass is
  the failure mode this amendment exists to prevent.
- The Criterion baseline lives in `build/engine/criterion`, which is gitignored
  and machine-local, so `perf-gate` is a two-step local ritual: a fresh checkout
  must run `just bench-micro-baseline` before `just bench-micro-check` can mean
  anything. That is by design (a baseline is only comparable to itself on one
  machine), not a defect to be re-discovered.
