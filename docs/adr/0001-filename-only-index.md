# ADR-0001: Index filenames only

Date: 2026-06-11 / Status: Accepted

## Decision

The index holds only filename, size, modified time, and attributes. No content index, no property/tag index, no preview.

## Rationale

- Speed and RAM come from the "index filenames only" tradeoff. The RAM gate is engine-only ≤110B/file (M2), which is orders of magnitude incompatible with a content index
- A content index inflates RAM by orders of magnitude (can reach 8GB-class)
- Filename-only indexing lands at ≈100B/file (the ≤110B RAM gate is the target derived from this)
- Real-world search syntax usage centers on substring, `ext:`, `path:`, `size:`, `dm:`; content:/regex: are niche (docs/RESEARCH.md)

## Consequences

- No search over file contents or meta-properties
- Under the same scope freeze, FTP/HTTP/ETP servers, FAT/exFAT/network drives (MVP), ReFS (MVP), and cross-platform support are also out of scope

## Re-examination triggers

- The core (filename-only index, content index excluded) is a permanent decision (canonical source: the "do-not list" in CLAUDE.md)
- Exception: the single point "volume-level only, no folder-walk index" was overridden by ADR-0024 (non-elevated scope index mode), justified by unlocking the non-elevated (corporate PC where elevation is forbidden) persona. The "filename-only index" core stays unchanged even under ADR-0024
