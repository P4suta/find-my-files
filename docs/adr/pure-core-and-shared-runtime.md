---
date: 2026-10-20
---

# Pure core and shared runtime

## Status

Accepted.
This partly supersedes ADR-0018's placement of the complete engine in `fmf-core`.
Its contract authority, compatibility rules, and two-seam limit remain in force.

## Context

The engine library combined deterministic disk decoding with Windows handles, persistence, threads, clocks, diagnostics, and shared query caches.
A library boundary alone did not make these operations pure.
Source-bound verification needs an enforceable boundary around the code that can be checked without an operating system.
The existing filename-index representation, query caches, checkpoint ordering, external contracts, and Windows-only product scope must survive the separation.

## Decision

`fmf-core` contains deterministic NTFS and USN byte decoding, object identities, civil-date arithmetic, WTF-8 conversion, and histogram bucket decisions.
It uses `no_std` with owned allocation, forbids unsafe code and explicit panic operations, and has no production dependencies.
Tests may use the standard library and property-test dependencies.
Kani harnesses stay beside the implementation they check.

`fmf-runtime` owns Windows acquisition, snapshot persistence, index storage and derived caches, parallel query execution, cancellation, diagnostics, and engine lifecycle management.
It calls the core decoders and rules directly.
The service, FFI boundary, and developer CLI use this one shared runtime.
The service dispatch and FFI entry points retain their existing boundary responsibilities.

`fmf-contract` remains the dependency-free authority for wire and ABI definitions.
The runtime and transport crates consume it directly.
The core does not copy its wire definitions.
The DLL name, golden corpus, persisted formats, service security model, and supported product platforms do not change.

The existing `SnapshotStore` and `JournalSource` seams move with engine orchestration.
No OS abstraction trait is added.
The existing internal date resolver stays with time-zone adaptation.

Renaming the complete old engine without extracting its deterministic implementation was rejected because it would leave the assurance boundary unenforced.
Rewriting index ownership, caches, and worker concurrency together was rejected because it would combine the boundary change with separate behavior and performance risks.

## Consequences

Cargo and the core crate attributes enforce the dependency and language boundary.
Repository invariant tests pin the core's dependency-free manifest and the runtime's capped interfaces.
The same native, golden, fuzz, interpreter, proof, administrator, and performance checks follow the moved implementation.
The proof receipt fingerprints both crates, and CI path selection covers changes to either.

The crate split adds one internal library and explicit re-exports for existing decoder paths.
Index storage and parallel search remain runtime responsibilities because they own shared caches and cancellation.
Future domain rules belong in the core when they can consume and return values without runtime state.
An observed performance regression or a domain rule that cannot cross this boundary without a new port requires reconsidering this decision.
