# ADR-0037: logfmt diagnostics, retention caps, and cross-process correlation

Date: 2026-06-30 / Status: Accepted (no wire-contract / golden / ABI change; the correlation reuses ids already on the wire)

## Context

Logging was already disciplined — `tracing` + a non-blocking daily appender + a `DiagLayer` fanning WARN+ to the diag ring and the UI on the engine side ([ADR-0018](0018-contract-single-source.md)'s `degrade!`), and a hand-rolled `FileLog` with crash markers and exception funnels on the app side. But against industry-standard structured logging four gaps remained:

1. **No retention cap (a real bug).** `tracing_appender::rolling::daily` never deletes old `engine.log.<date>` files — they accumulate forever. The app kept a single `.old` generation.
2. **Not structured.** Both sides wrote freeform human strings; `tracing`'s spans were wired but unused. Neither log was machine-parseable (grep/awk), and fields were not first-class.
3. **No cross-process correlation.** A single user query produces lines in both `app.log` and `engine.log` (two processes on the pipe path, two files even in-process on the FFI path) with nothing tying them together.
4. **No injection / redaction policy.** Query text and filenames — the product's *sensitive asset* (the whole index is filenames) — were logged verbatim, and nothing sanitised CR/LF or control characters out of values (log-injection / forged-line risk).

## Decision

Adopt one **logfmt** line schema as the canonical format for **both** languages, cap retention, and correlate the two logs using ids that already exist — so the contract is untouched.

1. **logfmt schema (the canonical surface).** Each line is `ts level area [field=value …] msg="…" [err="…"]`:
   - `ts` = RFC3339 with the local UTC offset (`2026-06-30T12:34:56.789+09:00`); `level` is a width-5 tag; `area` is the subsystem (`query`/`scan`/`snapshot`/`pipe`/…).
   - A value is emitted **bare** unless it contains a space, `=`, `"`, `\`, or a control char (`< 0x20`); then it is `"…"`-quoted with `"`→`\"`, `\`→`\\`, `\r`/`\n`/`\t`, and other control chars → `\uXXXX`. Values are capped at 1 KiB with a `…` marker.
   - Engine: a custom `tracing_subscriber::FormatEvent` (`LogfmtFormat` in `fmf-core::diag`) plus a matching `FormatFields` so span fields render the same way. App: a Serilog `ITextFormatter` (`LogfmtFormatter`).
2. **Retention caps.** Engine moves to `RollingFileAppender::builder().max_log_files(N)` (N = 14 for the resident service, 7 for FFI/CLI). App uses Serilog's File sink with `fileSizeLimitBytes = 5 MiB`, `rollOnFileSizeLimit`, `retainedFileCountLimit = 5`.
3. **Cross-process correlation — contract-unchanged.** The engine groups a request's log lines under a `qid` span (pipe: the frame `request_id`, already client-generated and echoed; FFI: an in-process counter). The per-query **"query served"** line — emitted once by each transport, where the result handle exists — carries `rid`: the resultId on the pipe, the boxed result handle's *address* on the in-process FFI path. The UI logs the same `rid` from `SearchAsync` on both transports. **`rid` is the universal app↔engine join key; `qid` adds intra-engine request grouping.** The query line is skipped for an unchanged idle USN requery, mirroring the UI's `RefreshInPlace`.
4. **Security.** The logfmt quoting *is* the log-injection defence (CR/LF can never escape a value). Query **text** is never logged — only `qlen` — because filenames/queries are the sensitive asset (redaction); the existing `%ProgramData%` DACL + no-telemetry posture still apply. The app facade (`FileLog`) takes scalar strings only and the ADR forbids Serilog destructuring (`{@obj}`) so an object graph can never be expanded into the log.
5. **C# adopts Serilog**, used directly (no `Microsoft.Extensions.Logging` / DI) to stay closest to the existing static `FileLog` facade. `FileLog` keeps its public surface (Info/Warn/Error + a new Debug and a structured `Event`) and routes through Serilog; the crash marker and `Tail` stay hand-rolled (a marker must survive a hard crash that never flushes the logger).

The change flow is one-directional and **stops short of the contract**: prose here → `LogfmtFormat`/`LogfmtFormatter` → both languages' tests green. `fmf-contract` / `fmf-proto` / `contract/golden` are not touched, proven by the golden suites staying green unmodified.

## Rationale

- **logfmt over JSON-lines**: the consumers are a human reading the file and the F12 "copy diagnostics" dump; logfmt keeps human readability while making fields machine-parseable. NDJSON would win only for an ingestion pipeline we explicitly do not have.
- **Reuse `request_id`/`rid` over a new field**: the pipe frame header already carries a client-generated `request_id`, and the result handle is already returned to the UI — correlation is therefore a *logging* change, not a *wire* change. Adding a `qid` to `FmfQueryOptions` (the alternative) would have been a golden-breaking contract change for no extra capability.
- **Span-based `qid`**: a per-request span means every line a request emits (including a `degrade!` warn mid-query) inherits the id automatically — no threading an id through every call site.
- **Transport-level "query served"**: `rid` is allocated by the transport, not by `Engine::query`; emitting the line there is the one place that has the trace *and* the handle, giving one fully-correlated line instead of two.

## Trade-off

The engine timestamp caches the local UTC offset once at process start (resolving the zone per line would dominate the formatter), so a DST boundary crossed mid-process stamps subsequent lines with the pre-transition offset — harmless for logs. On the FFI path the engine's `qid` counter and the UI's logs do not share a `qid` (no wire id exists in-process); they join on `rid` instead, which is sufficient. Query **errors** (which produce no result handle) are not `rid`-correlated; the engine still logs them under its `qid` span and the UI logs them separately.

## Rejected alternatives

- **OpenTelemetry / OTLP export, or any collector.** Rejected: the product is local-only with a permanent no-telemetry posture, the on-disk index is the sensitive asset behind a DACL, and the query hot path holds a single-digit-ms p99 budget — a collector/exporter on that path is unjustifiable. On-machine logs + the diag ring + `fmf_engine_stats` cover every need.
- **NDJSON (one JSON object per line).** Rejected for the *file* format: it halves human readability for a tool we do not run. (The diag ring is already `Serialize`-able if a JSON view is ever wanted.)
- **Adding `qid` to the query contract (`FmfQueryOptions` / `QueryTrace`).** Rejected: it breaks golden bytes / the C# DTO for a correlation we get for free from the existing `request_id` + `rid`.
- **`Microsoft.Extensions.Logging` abstraction in the app.** Rejected: it pressures a DI container into a hand-wired WinUI composition root; direct Serilog maps 1:1 onto the existing static `FileLog` calls.
- **`Serilog.Sinks.Async`.** Rejected: an async sink can lose the last lines on a hard crash, violating "don't go silent"; the synchronous File sink keeps them.

## Consequences

- **No wire-contract / golden / ABI change**; `init_diag` grows a `max_log_files` argument (internal). The engine `engine.log` filename gains a date (`engine.<date>.log`); F12 "open log folder" is unaffected, and `Tail` still reads the fixed `app.log`.
- New deps: `Serilog` + `Serilog.Sinks.File` (managed-only, ~1 MB; bundle size unaffected). No new Rust dependency — the formatter is hand-rolled on `std` + the index's existing civil-date math.
- A new counter is **not** added; no new `degrade!` path is introduced, so the `metrics.rs` / `COUNTER_NAMES` / `contract-gen` triple is untouched.

## Verification

- [x] Engine: `escape_value` unit tests (bare / quoting / **CRLF-injection folds to one line** / control-char `\uXXXX` / UTF-8-boundary truncation), `write_ts` RFC3339 shape, a full-line assembly test proving field order + span `qid`, and the `DiagLayer` area-precedence test. `just test` green incl. `golden_json` + `fmf-proto` golden **unmodified** (the contract-unchanged proof).
- [x] App: `LogfmtFormatterTests` (quoting, injection, exception `err=`, field ordering, level mapping) + the `FileLog` tail tests; `just test-app` green (482).
- [x] `just lint` (clippy all-groups deny) + `just fmt` green; C# `AnalysisMode=All` clean.
- [ ] Manual smoke: run a search, F12 "Copy diagnostics" shows logfmt `app.log` lines with `rid`; `engine.log` shows the same logfmt with the matching `rid` (and `qid` on the pipe path).
- [ ] Manual smoke: drive the log past the size/age caps and confirm `engine.<date>.log` stays ≤ N generations and `app_NNN.log` ≤ 5.

## Re-examination triggers

- If a genuine off-machine aggregation need ever appears (it should not, given the no-telemetry posture), revisit the NDJSON/OTLP rejections — but only behind an explicit opt-in.
- If query-**error** correlation becomes important, add an `rid`-less error line under the engine's `qid` span and a matching app-side field.
