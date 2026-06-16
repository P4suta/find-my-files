# ADR-0012: keep the default allocator + RecordArena for scan temporaries (mimalloc rejected)

Date: 2026-06-11 / Status: Accepted

## Decision

Do not swap out the global allocator; keep the default. Scan temporaries (the many ~1KiB records of the deferred/extension-record cache) stop using individual Boxes and are allocated contiguously into a slot-addressed RecordArena (scan.rs).

## Rationale

- Individual Boxes leave heap fragments after free that persist as a WS delta not visible in accounting. Going to RecordArena: real-C: steady-state WS 124.2→119.9MiB (−4.3MiB, consistent across 3 measurements)
- mimalloc A/B measured (fmf-cli feature gate, after a real-C: scan): steady-state WS **119.9MiB → ~380MiB (+260MB)**. mimalloc keeps freed segments in its own cache and does not return them to the OS, so scan temporaries sit there. Query p50 improves a few percent, but it is out of the question against the WS gate (≤110B/entry) → rejected
- RecordArena is a homegrown implementation with zero dependencies

## Impact

- RAM measurement is on the engine process WorkingSet basis (CLAUDE.md performance pass line), so the allocator's OS-return behavior lands directly on the gate — fix the premise that "small self-accounting" alone is not enough

## Re-examination trigger

- Only if mimalloc gains a stably-provided "return segments to the OS immediately" setting AND the WS gap (measured WS − self-accounting) widens beyond 10B/entry
