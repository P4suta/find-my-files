---
date: 2026-10-29
---

# Relocation-independent managed assembly identity

## Status

Proposed.

## Context

The artifact-derived SBOM reconciles shipped assemblies with the exact restored NuGet archives.
ReadyToRun preserves managed content while relocating MethodDef and FieldRVA addresses.
The candidate using CommunityToolkit.Mvvm 8.4.2 reproduced a rejection because the original metadata digest included MethodDef addresses.
[ECMA-335, Partition II](https://www.ecma-international.org/wp-content/uploads/ECMA-335_6th_edition_june_2012.pdf) defines the metadata tables, method bodies, and initialized field data.
The [.NET ReadyToRun format](https://github.com/dotnet/runtime/blob/main/docs/design/coreclr/botr/readytorun-format.md) describes the retained IL and metadata.

## Decision

The SBOM normalizer derives a versioned managed-content digest from validated PE mappings.
The digest retains every metadata stream byte except validated MethodDef and FieldRVA addresses.
It binds each normalized address to its referenced IL body or initialized field bytes, in metadata row order.
It also binds managed resources.
Fixed table-header fields remain in the digest even though [.NET's metadata reader](https://github.com/dotnet/runtime/blob/v10.0.10/src/libraries/System.Reflection.Metadata/src/System/Reflection/Metadata/MetadataReader.cs) ignores their values while decoding.
Method headers, IL instructions, exception sections, field signatures, and declared field sizes remain part of the identity.
Physical placement and alignment padding between referenced structures don't determine managed-content identity.
Unsupported layouts, invalid targets, truncated structures, and arithmetic overflow fail closed.
The implementation and regression cases live in [the SBOM normalizer](../../xtask/src/sbom.rs) and its managed-content parser.

The digest supplements the existing package archive content hash, declared asset path and versions, actual shipped file hash, and bundle source identity.
It doesn't establish the correctness of generated native code or replace the trusted build and signing boundaries.
The pinned .NET compiler remains responsible for producing native code from the retained managed content.
The production range validator has a source-bound proof obligation.
Native tests cover CLR formats and the actual published dependency boundary, beyond that scalar proof's semantics.

## Consequences

ReadyToRun remains enabled without an assembly-specific exception or an advisory suppression.
Dropping address bytes without checking their referents would permit undetected IL or static-data changes, so that alternative is rejected.
Disabling ReadyToRun would change shipping performance to accommodate an incorrect identity check, so that alternative is rejected.
The normalizer supports the admitted ECMA-335 layouts explicitly and rejects additional layouts until their identity rules and regressions are implemented.
Future changes to Crossgen2 output require revisiting the supported layouts when an actual candidate fails reconciliation.
