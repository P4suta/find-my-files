# ADR-0023: First-class regular expressions (literal-prefilter driven + compile limits; trigram still not adopted)

Date: 2026-06-15 / Status: Adopted

## Decision

Promote regex search from hidden syntax (`regex:` typed by hand) to a first-class feature. Three parts:

1. **Whole-query mode as a contract flag**. Add `regex_mode:u32` (16→20B) to `FmfQueryOptions`. bit0 = interpret the entire query as a single regex, bit1 = scope (0=name / 1=full path), upper bits reserved 0. The UI switches via a gear-menu toggle plus a "target" submenu. Hand-typed `regex:` coexists as before. Expressing this via query rewriting is rejected (`|`/`!`/`"`/whitespace would be doubly interpreted as the parser's AND/OR/NOT, making whole-query mode impossible to express safely).
2. **literal-prefilter driven**. Run the regex through `regex_syntax` prefix/suffix literal extraction, feed the required literal into the existing folded-pool linear sweep (`Driver::Sub`) to narrow candidates, and confirm with the regex body as residual. Name scope only. Cases where extraction fails (`\d+`, leading `.*`, alternations with no common factor) go full-scan + rayon. The single most recent compile is cached inside the engine (skips recompilation on USN re-query / RefreshInPlace).
3. **Compile limits**. Set `RegexBuilder` `size_limit`/`dfa_size_limit` = 1MiB each. Overflow yields `regex::Error::CompiledTooBig` → existing `CompileError::Regex` → `FMF_E_QUERY_SYNTAX(5)`. No new error code is added.

Because this is an incompatible wire change, bump the pipe name `fmf-engine-v1`→`v2` and raise `ABI_VERSION`/`PROTOCOL_VERSION` to 2 (per the rule "incompatible changes bump the name too").

## Rationale

- **Consistency with ADR-0002 (most important)**: the prefilter is **not a trigram inverted index**. What ADR-0002 rejected was "maintaining n-gram postings as a resident index" (RAM +10–15B/file, diff maintenance per USN batch). This prefilter **only extracts literals from the regex at query-compile time and linearly sweeps the existing pool** — zero resident index, zero RAM increment, zero USN diff maintenance. It merely applies ADR-0002's core "linear pool sweep" to regex too, and does not contradict the decision.
- **Linear-time guarantee**: the Rust `regex` crate uses finite automata (lazy-DFA/Pike VM, no backtracking), so **match execution is linear in input length → ReDoS runtime exponential blowup is structurally absent** (corroborated: docs/RESEARCH.md). The remaining attack surface is **compile time/memory** (expansion of huge patterns). The index is filenames only (p99 ≈110B), and legitimate name regexes are on the order of tens of bytes and never reach a 1MiB program = without catching legitimate users, this tips toward "politely reject" rather than compiling a malicious pattern inside the elevated service. Set stricter than the defaults (10/2 MiB).
- **Prefilter correctness**: what prefix/suffix extraction returns is "the literal that every match has at its start (end)." Its longest common factor `S` exists contiguously in every match → exists in the name. Folding `S` and sweeping the folded pool yields a superset in both case modes (the original matching implies folded matching, length-preserving), and the regex residual confirms exactly. Zero false negatives is the one inviolable correctness requirement, ensured by an oracle differential test (prefilter == full-scan, name/path × case3).
- **Reason for the v2 bump**: in the past, "after a revert, an old-protocol service binary kept running and corrupted queries" occurred. 16→20B is incompatible, and bumping the pipe name makes old v1 services **unreachable** (no accident of misreading a 20B request as 16B+text), doubly guarded with Hello version matching.

## Measurements (2026-06-15, real C: 1.6M entries / synthetic 1M criterion)

- **Regexes where the prefilter works stay within the hard line**: `regex:win.*\.dll` (prefix "win") = p99 **9.1ms** @1.6M. In micro too, `win.*\.dll` 8.5ms / `\.dll$` (suffix) 7.1ms. Existing queries (substring/wildcard/ext/size) are all non-regressing.
- **literal-less regex goes full-scan**: micro `regex:[0-9]{4}x` @1M = **28.7ms** (within the 50ms hard line). Real C: `regex:[0-9]{6,}` is p99 ~51ms at 1.6M entries — it meets spec scale (1M) but, being linear in count, exceeds it at over-spec volumes. Whereas memmem substring full-scan (single char `a`/`e`) is 6-8ms even at 1.6M, regex matching is ~9x heavier, so only the literal-less class grows linearly and crosses 50ms.
- **Bench policy**: the gated set on real volume (`P99_BUDGET_US=50ms` hard) holds only `regex:win.*\.dll`, which the prefilter can guarantee. The literal-less worst case is measured and recorded in criterion micro (`query/regex_scan`, ungated) — gating it on a fixed 50ms line would fail merely because "the machine has more files than spec" (not concealment, but gating only the range we can guarantee and measuring the worst case separately + documenting it in this ADR).
- Streaming-regex optimization over the whole pool is rejected as **unsound** for `^`/`$` anchors, cross-entry-boundary spans, and greedy matches (full-scan of literal-less regex is the accepted filename-only/no-index tradeoff).

## Consequences

- Contract evolution (ADR-0018 flow): ARCHITECTURE.md → `fmf-contract` (pod/options/versions) → `FMF_BLESS=1` golden recapture → `just contract-gen` → both-language tests green. `contract/golden/query_req_*.bin` is recaptured at 20B.
- One line in `docs/SECURITY.md` threat #5 (regex compile compute DoS → reject via limit).
- Kill switch `FMF_REGEX_PREFILTER=0` (force fallback to full-scan; a field recovery valve of the same kind as `FMF_QUERY_CACHE`).
- Observability: on prefilter success `QueryTrace.driver` is `pool-scan`; on extraction failure it is `full-scan`.
- C# side: `RegexMode`/`Scope` on `SearchOptions`, 20B encoding in `PipeProtocol`, `AppSettings` persistence, gear-menu UI, `RegexHighlighter` (.NET re-match; if it drifts, do not highlight).

## Re-examination triggers

1. literal-less regex full-scan **exceeds the 50ms hard line @1M** (currently ~29ms, inside. If it breaks at 1M scale, consider a separate path for prefilter-incapable patterns — e.g., a sound subset of pool streaming or a required-byte-class prefilter). The 51ms at 1.6M is linear growth from over-spec volume and is not itself a trigger.
2. Real demand for path-scope regex is high and full-scan breaks p99 → consider path prefilter via name-portion anchor extraction.
3. Measurement shows typical whole-query queries skew literal-less → only then re-evaluate ADR-0002 trigram (AND'd with all triggers of that ADR).
4. A real report of a legitimate user hitting the 1MiB compile limit.
