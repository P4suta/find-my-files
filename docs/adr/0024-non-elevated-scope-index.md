# ADR-0024: Remove the non-elevated scope index

Date: 2026-07-26 / Status: Adopted

The folder-walk + `ReadDirectoryChangesW` fallback was removed. It duplicated
scan, freshness, persistence, configuration, and onboarding while providing
weaker coverage than the product's defining NTFS `$MFT` + USN path.

Production indexing now has one model: an asInvoker UI talks to the on-demand
service; an already-elevated `--engine=inproc` remains the development/recovery
fallback. If the service is absent, the UI offers its one-time installation
instead of building a second per-folder engine.

Scope *exclusions* go with it. They existed to prune the folder walk, and there
is no longer a folder-walk ingest path to prune; excluding a path from results
remains available where it always belonged, as query syntax. (This absorbs the
separately recorded exclusion decision, which had no content of its own once
the walk was gone.)
