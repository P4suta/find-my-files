# ADR-0019: Focused mode (focused search) is a pure query rewrite in the UI layer

Date: 2026-06-12 / Status: Accepted

## Decision

For the request "the files a general user looks for are limited in both directory and format", focused mode is
realized as a **pure query rewrite in the UI layer** (`ViewModels/FocusedQueryRewriter.Compose` — static, no side effects).
It does not touch the engine (Rust), the wire contract, or the index at all.

- Split the user query on top-level `|` (do not split `|` inside quotes — same quoting rules as the engine's
  tokenizer), append config-derived suffixes to each OR group, and rejoin:
  - Excluded paths: for each entry p, `!path:"p"` (quoted negation — noise areas such as `\windows\`)
  - Format whitelist: `ext:e1;e2;…` as **one term** (the value of `ext:` is OR semantics, so do not add more terms)
- **Collision avoidance (the user's explicit intent always wins)**: if a group contains `ext:`/`regex:`, do not append the ext
  whitelist; if it contains `path:` or `\`, do not append excluded paths. The check is a simple substring test on the group
  string (over-matching only ever falls toward "skip the append" = the safe side).
- An empty query is returned empty (the rule "do not throw an empty query at the engine" remains owned by the Orchestrator).
  An excluded-path config value containing `"` is unescapable in the query language, so it is ignored + warned (first time only).
- Settings live in `%APPDATA%\find-my-files\settings.json` (UI-owned): `focused_search` (default **true**) /
  `focused_exclude_paths` / `focused_extensions`. The UI is a ToggleButton next to the search box;
  a toggle change is a filter-originated re-query (`RequeryOrigin.Filter` = reset to top).
- `SearchOrchestrator.FocusedSearch` defaults to **false** (to keep existing tests and existing behavior intact).
  Only the product wiring (MainViewModel) feeds in the settings' true.

## Rationale

- **Everything is expressible in the existing query language**: `ext:` is a 1-term OR (`;`-separated), `path:` supports
  quotes + `!` negation, `|` is an OR group (fmf-core/src/query/ast.rs). No new operator or new filter mechanism is needed.
- **The residual cost is proportional to hit count**: the engine's linear sweep is unchanged, and the appended terms only work
  to reduce candidates. The rewrite itself is string concatenation (per keystroke, a few µs) and does not load the latency budget.
- **No engine contact = no perf-gate needed**: since fmf-core is untouched, it can ship without the elevated-bench / regression-gate
  ritual. Wire bytes, contract, and golden corpus are also unchanged.

## Rejected alternatives

- **In-engine preset filter (index bit)**: precomputing a "noise-area flag" per entry would make the residual cost nearly zero,
  but it incurs an index layout change (RAM budget, snapshot version), a full recompute on config change, and an ownership cross
  between UI settings and the service-owned index. This is an optimization to consider after the rewrite approach is measured to
  be slow, not a cost to pay upfront.
- **Ranking (relevance-order sort)**: the proper way to get "exactly a few hits" is scoring, not filtering, but it needs an
  entire scoring foundation (feature values such as usage frequency, recency, and path depth, plus a permutation cache) and is
  not orthogonal to the current lazy-sort permutation (ADR-0006). **Noted as future work** — not built while filtering suffices.

## Consequences

- Change surface: `FocusedQueryRewriter` (new) + 3 `AppSettings` keys + one rewrite point in `SearchOrchestrator`
  + a ToggleButton in MainPage. Engine, contract/golden, and Generated are unchanged.
- The query notation in the F12 panel/logs is **post-rewrite** (the string the engine actually saw) — when investigating, keep
  the two lists in settings.json in mind.
- Because it is ON by default, triage "the file should exist but does not show up" inquiries by first turning the toggle OFF
  (the existence of exclusions is already noted in the tooltip).

## Re-examination triggers

- Focused-ON search p99 > 50ms (exceeds the performance pass line — re-evaluate an in-engine preset filter).
- When filtering's "exactness" falls short and ranking (a scoring foundation) becomes necessary.
