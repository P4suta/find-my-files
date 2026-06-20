# Troubleshooting

A practical map from a symptom to its cause and fix. The authoritative error-code
table and the UI's reaction to each code live in
[ARCHITECTURE](ARCHITECTURE.md#ffi-contract-c-abi) — this page is the field guide,
not a second source of truth.

## Look here first

1. **Logs.** Engine: `%ProgramData%\find-my-files\logs\engine.log` (daily rotation). App: `%APPDATA%\find-my-files\logs\app.log` (one-generation rotation at 2 MB).
2. **The F12 panel** (see below) — the fastest way to see the last query's stage breakdown, the engine counters, RAM, and recent errors without leaving the app.
3. **`just doctor`** — if the problem is your *environment* (wrong toolchain, missing lefthook, stray `target/`, drifted contract) rather than the running app.

## Turning up the engine log: `FMF_LOG`

The engine filter is a [`tracing_subscriber` `EnvFilter`](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html),
read from the `FMF_LOG` environment variable. It overrides the default level.

```
FMF_LOG=debug              # everything at debug and above
FMF_LOG=fmf_core=trace     # only fmf-core, at trace
FMF_LOG=info,fmf_core::query=debug   # info globally, debug for the query module
```

Set it in the shell that launches the engine (`just service-dev`, `--engine=inproc`,
or any `fmf-cli` recipe). For the installed service the engine logs to
`engine.log` regardless; restart the service after changing the variable.

`cargo run -p fmf-cli -- diag` prints the running version, the resolved log paths,
and the in-memory diagnostic ring — a quick first stop when a recipe misbehaves.

## The F12 performance panel

Press **F12** in the app (or the gear/debug menu) to toggle the diagnostics panel.
It is diagnostic chrome, not app data, and shows:

- **Last query trace** — the per-stage breakdown of the most recent query (or nothing for an empty query).
- **Engine stats** — the counter snapshot, RAM, and the most recent errors.
- **Recent latency history** — the last 64 query totals (µs).
- **Engine mode** — which transport is live: `fake`, `in-proc`, or `pipe`. (This precise vocabulary lives here on purpose; it confused end users on the gear menu.)

When you file a perf or correctness report, a screenshot of F12 plus the relevant
`engine.log` lines is the ideal payload.

## Error codes — symptom → cause → fix

The codes are **append-only and never renumbered** (the contract tests pin the
values). Full table: [ARCHITECTURE](ARCHITECTURE.md#ffi-contract-c-abi). The
`fmf` CLI maps the same numbers onto its **process exit code** (`$LASTEXITCODE`),
and `--format json` repeats them in the error envelope (`error.code` /
`error.code_num`) — so a script can branch on the failure class
([ADR-0026](adr/0026-cli-surface-polish.md)).

| Code | What it means | What to do |
|---|---|---|
| `FMF_E_INVALID_ARG` (1) | A bad argument crossed the FFI/pipe boundary | A caller bug — check the `fmf_last_error` detail (it is thread-local). |
| `FMF_E_STALE` (2) | A structural generation change invalidated a result handle | Expected during compaction/rescan; the UI auto-reissues the same query. If you see it in a test, you held a page across a structural change. |
| `FMF_E_NOT_ADMIN` (3) | Insufficient elevation for `$MFT`/USN access | Run from an elevated shell (`just service-dev`, `--engine=inproc`) or install the service. The app shows an InfoBar and offers an elevated relaunch. |
| `FMF_E_VOLUME` (4) | Volume not supported / not readable | Check it is a fixed NTFS volume — FAT/exFAT/network/ReFS are out of scope. |
| `FMF_E_QUERY_SYNTAX` (5) | Bad query, or a regex over the 1 MiB compile cap | Fix the query; a giant regex is rejected by design (DoS guard, [ADR-0023](adr/0023-regex-first-class.md)). |
| `FMF_E_IO` (6) | An I/O error while reading the volume or snapshot | Check `engine.log` for the underlying OS error. |
| `FMF_E_LOCKED` (7) | Another process holds the index-dir writer lock | The single-writer invariant. Usually: the service is running and you started `--engine=inproc`. Run `just service-stop`, then retry. The lock auto-releases when the holder's handle vanishes, so it never goes stale. |
| `FMF_E_PANIC` (99) | A panic was caught at the engine boundary | The engine survives (the dispatcher is a `catch_unwind` firewall). Grab the `engine.log` stack and the `fmf_last_error` detail and file it. |

## Common situations

- **"Service is running. Use in-proc after `just service-stop`."** — You hit `FMF_E_LOCKED`. Stop the installed service (or the foreground `just service-dev`) before launching the in-proc engine; only one writer may own the index dir.
- **The app starts but the engine looks dead / shows `fake`.** — On a fatal init failure the app deliberately falls back to `FakeEngineClient` rather than crashing silently. Check `app.log` for the init error and confirm the service is installed/running (`just service-status`).
- **A push fails in a pre-push hook.** — That is the quality gate doing its job; never bypass it with `--no-verify`. If it is clippy/test, fix the finding. If it is a stray `xtask/target` or contract drift, run `just doctor` to pinpoint it (and `just contract-gen` to fix drift).
- **`just index` / `just bench` / `just service-dev` error immediately.** — These need an **elevated** terminal (MFT/USN access). See the recipes marked *elevated* in the [justfile](https://github.com/P4suta/find-my-files/blob/main/justfile).
- **Names show replacement characters (�).** — Known limitation: names with unpaired surrogates are searchable but displayed with replacement characters.

## See also

- [DEVELOPMENT](DEVELOPMENT.md) — the error-handling conventions (`degrade!`, `task.Forget`, the counter three-part set).
- [SECURITY](SECURITY.md) — the threat model behind the pipe DACL and the data-dir ACL.
