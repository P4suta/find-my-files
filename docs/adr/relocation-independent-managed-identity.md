---
date: 2026-10-29
---

# Relocation-independent managed assembly identity

## Status

Proposed.

## Context

ReadyToRun relocates MethodDef and FieldRVA addresses while retaining managed content.
Address-sensitive digests reject genuine assemblies.

## Decision

Derive a versioned managed-content digest from validated PE mappings.
Normalize only validated relocation addresses.
Bind referenced IL bodies and initialized field data in metadata order, plus resources and all other metadata bytes.
Reject unsupported layouts, truncation, invalid targets, and overflow.
Retain package hashes, asset paths, versions, shipped hashes, and bundle identity.
[Format analysis](https://github.com/P4suta/find-my-files/blob/198680fcf43d9de21d7be88c4cd270146af94a2e/docs/adr/relocation-independent-managed-identity.md) records ECMA-335 and ReadyToRun references.

## Alternatives

Discarding addresses without checking referents hides changed content.
Disabling ReadyToRun sacrifices shipping performance to accommodate the digest.

## Consequences

Supported layouts require explicit parsing.
The digest supplements trusted compilation and signing; it does not verify generated native code.

## Confirmation

Run parser regressions, the production range proof, and SBOM reconciliation against the published bundle.
Changed method, field, resource, or metadata bytes must fail reconciliation.
Reconsider when changed Crossgen2 output exposes an unsupported layout.
