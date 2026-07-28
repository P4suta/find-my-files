# ADR-0043: Monotonic FFI allocation-owner IDs

Date: 2026-07-26 / Status: Accepted. Superseded only in its version numbers by
[ADR-0044](0044-cooperative-query-cancellation-and-presentation-basis.md), which
raised the FFI ABI again; the owner-ID ownership rule and the descriptor layouts
decided here are unchanged. Current values are `fmf-contract::versions`, not
this text.

`FmfPage` and `FmfBlob` carry a nonzero `owner_id: u64`. Their free exports
accept only that ID:

```c
int32_t fmf_page_free(uint64_t owner_id);
int32_t fmf_blob_free(uint64_t owner_id);
```

Page and blob owners remain in separate live-allocation registries, while their
IDs come from one process-wide monotonic namespace. ID zero is the
no-allocation/free-no-op sentinel. Unknown, already-freed, forged, stale, and
cross-kind IDs return `FMF_E_INVALID_ARG` without dereferencing caller memory.
IDs are never reused; exhaustion fails closed with `FMF_E_IO`.

The previous address-keyed registries rejected ordinary forged, cross-kind, and
double frees, but could not distinguish an old pointer from a newer allocation
placed at the same recycled address (ABA). Reading a cookie through the stale
pointer would itself be undefined behavior. Returning a generation-bearing
owner ID and freeing by ID alone removes foreign addresses from ownership
transfer and closes that gap.

This is an incompatible FFI-only POD/signature change, so `ABI_VERSION` is 4:
`FmfPage` is 40 bytes with `owner_id` at offset 32, and `FmfBlob` is 24 bytes
with `owner_id` at offset 16. The named-pipe page/blob encoding, pipe name, and
`PROTOCOL_VERSION=3` are unchanged. Runtime `HelloResp.abi_version` naturally
reports 4. The shared golden `HelloResp` is deliberately a literal
version-1 representative rather than a snapshot of current constants, so its
wire bytes remain unchanged.

Contract tests pin layout/signatures and prove zero no-op, forged/double and
cross-kind rejection, and the ABA property: freeing an old ID after allocating
a replacement cannot release the replacement.
