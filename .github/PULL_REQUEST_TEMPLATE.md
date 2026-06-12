## Summary

<!-- What does this change, and why? -->

## Checklist

- [ ] PR title follows [Conventional Commits](https://www.conventionalcommits.org/) (`feat:`, `fix:`, `perf:`, `docs:`, …) — release-please reads it on squash-merge
- [ ] `just verify` passes (fmt-check + lint + test + test-app)
- [ ] If `fmf-core` changed: ran `just perf-gate` (elevated, cool machine) and noted the result
- [ ] If the contract changed: ran `just contract-gen` and committed the regenerated bindings
- [ ] No hand-edits to `app/FindMyFiles/Engine/Generated/` (regenerate instead)

See [CONTRIBUTING.md](../CONTRIBUTING.md).
