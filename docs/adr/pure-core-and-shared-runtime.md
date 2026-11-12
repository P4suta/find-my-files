---
date: 2026-10-20
---

# Pure core and shared runtime

## Status

Accepted.
Partly supersedes [ADR-0018](0018-contract-single-source.md).

## Context

Windows resources and deterministic decoding shared one crate.
Verification needs an enforceable pure boundary.

## Decision

fmf-core owns deterministic decoding, identities, dates, WTF-8 conversion, and histogram rules.
It uses no_std, permits allocation, forbids unsafe code and explicit panics, and has no production dependencies.
fmf-runtime owns Windows resources, persistence, indexes, caches, queries, workers, cancellation, and diagnostics.
Service, library, and command-line boundaries use the shared runtime.
Keep fmf-contract authoritative and retain only SnapshotStore and JournalSource.

## Alternatives

Renaming the combined crate leaves effects unconstrained.
Rewriting caches and concurrency during extraction combines independent risks.

## Consequences

One internal crate and re-exports are added.
External contracts, stored formats, golden bytes, the fmf_engine name, and Windows support remain unchanged.

## Confirmation

Architecture guards enforce dependencies and seam limits.
Run native, golden, proof, interpreter, fuzz, administrator, and performance checks over the moved implementation.
[Migration details](https://github.com/P4suta/find-my-files/blob/198680fcf43d9de21d7be88c4cd270146af94a2e/docs/adr/pure-core-and-shared-runtime.md) preserve the original scope.
Reconsider after a measured regression or a domain rule requiring another port.
