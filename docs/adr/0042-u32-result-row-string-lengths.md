# ADR-0042: u32 result-row string lengths

Date: 2026-07-26 / Status: Accepted. Superseded only in its version numbers by
[ADR-0043](0043-ffi-allocation-owner-ids.md) (FFI ABI) and
[ADR-0044](0044-cooperative-query-cancellation-and-presentation-basis.md) (pipe
protocol and pipe name); the row layout decided here is unchanged. Current
values are `fmf-contract::versions`, not this text.

`FmfRow.name_len` and `parent_path_len` are `u32`; the row is 56 bytes with an explicit zero reserved tail word. This bumps both the FFI ABI and named-pipe protocol to 3 (`fmf-engine-v3`). Golden frames are intentionally recaptured.

The former `u16` lengths silently wrapped parent paths above 65,535 WTF-8 bytes even though Windows permits longer extended-length paths. Rejecting those paths would make valid NTFS entries unsearchable, so widening the shared row is the only lossless choice. Path reconstruction is separately bounded at the maximum possible WTF-8 size of a valid 32,767-unit NT path and rejects cycles, out-of-range parents, and larger corrupt acyclic graphs before materialization.

Both codecs validate every blob window and the zero reserved field. Page row count, encoded payload size, and indexing volume count remain contract-bounded before allocation.
