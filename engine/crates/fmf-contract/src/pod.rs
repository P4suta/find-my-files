//! `#[repr(C)]` POD types shared by the FFI (by layout) and the pipe wire
//! (by explicit little-endian serialization in fmf-proto).
//!
//! The `const`
//! blocks pin every size and offset at `cargo check` time — the same pins
//! fmf-ffi's `contract_tests` re-assert at run time as an independent
//! tripwire, and gen-contract radiates to C# `[FieldOffset]` values.

use core::mem::{align_of, offset_of, size_of};

use crate::volume;

/// 48-byte result row, no internal padding. Offsets index into the page's
/// trailing string blob (WTF-8). Mirrored by C# `LayoutKind.Explicit`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FmfRow {
    /// Stable engine-internal entry handle for this result.
    pub entry_ref: u64,
    /// NTFS File Reference Number of the entry.
    pub frn: u64,
    /// File size in bytes.
    pub size: u64,
    /// Last-modified time (Windows FILETIME ticks: 100 ns since 1601).
    pub mtime: i64,
    /// Byte offset of the file name into the page's trailing string blob.
    pub name_off: u32,
    /// Byte offset of the parent path into the page's trailing string blob.
    pub parent_path_off: u32,
    /// Packed entry attribute flags (hidden/system/directory bits).
    pub flags: u32,
    /// File name length in bytes within the string blob (WTF-8).
    pub name_len: u16,
    /// Parent path length in bytes within the string blob (WTF-8).
    pub parent_path_len: u16,
}

impl FmfRow {
    /// Size of one row in bytes (48).
    pub const LEN: usize = size_of::<Self>();
}

const _: () = {
    assert!(size_of::<FmfRow>() == 48);
    assert!(align_of::<FmfRow>() == 8);
    assert!(offset_of!(FmfRow, entry_ref) == 0);
    assert!(offset_of!(FmfRow, frn) == 8);
    assert!(offset_of!(FmfRow, size) == 16);
    assert!(offset_of!(FmfRow, mtime) == 24);
    assert!(offset_of!(FmfRow, name_off) == 32);
    assert!(offset_of!(FmfRow, parent_path_off) == 36);
    assert!(offset_of!(FmfRow, flags) == 40);
    assert!(offset_of!(FmfRow, name_len) == 44);
    assert!(offset_of!(FmfRow, parent_path_len) == 46);
};

/// FFI page: one contiguous engine-allocated block (row array + string
/// blob). Pointers, so FFI-only — the pipe sends rows and blob inline.
#[repr(C)]
pub struct FmfPage {
    /// Number of rows in the `rows` array.
    pub row_count: u32,
    /// C ABI padding (reserved; always 0).
    #[expect(clippy::pub_underscore_fields, reason = "C ABI padding/reserved field")]
    pub _pad: u32,
    /// Pointer to the `row_count`-element array of [`FmfRow`].
    pub rows: *const FmfRow,
    /// Pointer to the trailing string blob the row offsets index into.
    pub blob: *const u8,
    /// String blob length in bytes.
    pub blob_len: u32,
    /// C ABI padding (reserved; always 0).
    #[expect(clippy::pub_underscore_fields, reason = "C ABI padding/reserved field")]
    pub _pad2: u32,
}

const _: () = {
    assert!(size_of::<FmfPage>() == 32);
    assert!(align_of::<FmfPage>() == 8);
    assert!(offset_of!(FmfPage, row_count) == 0);
    assert!(offset_of!(FmfPage, rows) == 8);
    assert!(offset_of!(FmfPage, blob) == 16);
    assert!(offset_of!(FmfPage, blob_len) == 24);
};

/// Engine-allocated UTF-8 JSON payload (stats, query traces); release with
/// `fmf_blob_free`.
#[repr(C)]
pub struct FmfBlob {
    /// Pointer to the UTF-8 JSON payload bytes.
    pub data: *const u8,
    /// Payload length in bytes.
    pub len: u32,
    /// C ABI padding (reserved; always 0).
    #[expect(clippy::pub_underscore_fields, reason = "C ABI padding/reserved field")]
    pub _pad: u32,
}

const _: () = {
    assert!(size_of::<FmfBlob>() == 16);
    assert!(align_of::<FmfBlob>() == 8);
    assert!(offset_of!(FmfBlob, data) == 0);
    assert!(offset_of!(FmfBlob, len) == 8);
    assert!(offset_of!(FmfBlob, _pad) == 12);
};

/// POD event payload — FFI callback argument and pipe event-push body
/// (32 bytes). `volume` is the zero-padded UTF-8 drive label ("C:").
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FmfEvent {
    /// [`crate::events::EventKind`] as u32.
    pub kind: u32,
    /// C ABI padding (reserved; always 0).
    #[expect(clippy::pub_underscore_fields, reason = "C ABI padding/reserved field")]
    pub _pad: u32,
    /// Entry count carried by the event (e.g. indexed entries so far).
    pub entries: u64,
    /// Zero-padded UTF-8 drive label this event concerns (e.g. "C:").
    pub volume: [u8; 16],
}

impl FmfEvent {
    /// Size of the event in bytes (32).
    pub const LEN: usize = size_of::<Self>();

    /// Builds an event, encoding `volume` into the fixed 16-byte label.
    #[must_use]
    pub fn new(kind: u32, entries: u64, volume: &str) -> Self {
        Self {
            kind,
            _pad: 0,
            entries,
            volume: volume::encode_label(volume),
        }
    }

    /// Decodes the fixed 16-byte label back to a trimmed drive string.
    #[must_use]
    pub fn volume_str(&self) -> &str {
        volume::decode_label(&self.volume)
    }
}

const _: () = {
    assert!(size_of::<FmfEvent>() == 32);
    assert!(align_of::<FmfEvent>() == 8);
    assert!(offset_of!(FmfEvent, kind) == 0);
    assert!(offset_of!(FmfEvent, _pad) == 4);
    assert!(offset_of!(FmfEvent, entries) == 8);
    assert!(offset_of!(FmfEvent, volume) == 16);
};

/// Query options — 20 bytes, no padding, LE on the wire. Field values are
/// the [`crate::options`] enums as u32.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FmfQueryOptions {
    /// Result column to order by ([`crate::options::SortKey`] as u32).
    pub sort: u32,
    /// Nonzero sorts descending; 0 ascending.
    pub desc: u32,
    /// Case-matching policy ([`crate::options::CaseMode`] as u32).
    pub case_mode: u32,
    /// Nonzero shows hidden/system entries (default-excluded otherwise).
    pub include_hidden_system: u32,
    /// Packed regex mode (ADR-0023): bit0 = treat the whole query as one
    /// regex, bit1 = scope (0 = file name, 1 = full path). 0 = off (the
    /// query parses normally; `regex:` per-term syntax still works). Higher
    /// bits are reserved 0, so a future flag keeps `LEN` at 20.
    pub regex_mode: u32,
}

impl FmfQueryOptions {
    /// Size of the options struct in bytes (20).
    pub const LEN: usize = size_of::<Self>();
}

const _: () = {
    assert!(size_of::<FmfQueryOptions>() == 20);
    assert!(align_of::<FmfQueryOptions>() == 4);
    assert!(offset_of!(FmfQueryOptions, sort) == 0);
    assert!(offset_of!(FmfQueryOptions, desc) == 4);
    assert!(offset_of!(FmfQueryOptions, case_mode) == 8);
    assert!(offset_of!(FmfQueryOptions, include_hidden_system) == 12);
    assert!(offset_of!(FmfQueryOptions, regex_mode) == 16);
};

/// FFI volume status. `state` is [`crate::options::VolumeState`] as u32.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct FmfVolumeStatus {
    /// Zero-padded UTF-8 drive label this status concerns (e.g. "C:").
    pub label: [u8; 16],
    /// Volume lifecycle state ([`crate::options::VolumeState`] as u32).
    pub state: u32,
    /// C ABI padding (reserved; always 0).
    #[expect(clippy::pub_underscore_fields, reason = "C ABI padding/reserved field")]
    pub _pad: u32,
    /// Number of indexed entries on this volume.
    pub entries: u64,
}

const _: () = {
    assert!(size_of::<FmfVolumeStatus>() == 32);
    assert!(align_of::<FmfVolumeStatus>() == 8);
    assert!(offset_of!(FmfVolumeStatus, label) == 0);
    assert!(offset_of!(FmfVolumeStatus, state) == 16);
    assert!(offset_of!(FmfVolumeStatus, entries) == 24);
};

/// 16-byte little-endian pipe frame header. `to_bytes`/`from_bytes` are
/// pure byte conversions — the `MAX_PAYLOAD_LEN` *policy* lives in
/// fmf-proto's `decode_header`/`read_frame`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    /// Payload length in bytes (the header itself excluded).
    pub len: u32,
    /// Frame operation code (request/response/event kind).
    pub opcode: u16,
    /// Per-opcode bit flags.
    pub flags: u16,
    /// Request/response correlation; 0 on event pushes.
    pub request_id: u32,
    /// Status code ([`crate::codes`]); meaningful on responses only.
    pub status: i32,
}

impl FrameHeader {
    /// Size of the frame header in bytes (16).
    pub const LEN: usize = size_of::<Self>();

    /// Serializes the header to its 16-byte little-endian wire form.
    #[must_use]
    pub fn to_bytes(self) -> [u8; Self::LEN] {
        let mut b = [0u8; Self::LEN];
        b[0..4].copy_from_slice(&self.len.to_le_bytes());
        b[4..6].copy_from_slice(&self.opcode.to_le_bytes());
        b[6..8].copy_from_slice(&self.flags.to_le_bytes());
        b[8..12].copy_from_slice(&self.request_id.to_le_bytes());
        b[12..16].copy_from_slice(&self.status.to_le_bytes());
        b
    }

    /// Parses a header from its 16-byte little-endian wire form.
    #[must_use]
    pub const fn from_bytes(b: &[u8; Self::LEN]) -> Self {
        Self {
            len: u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            opcode: u16::from_le_bytes([b[4], b[5]]),
            flags: u16::from_le_bytes([b[6], b[7]]),
            request_id: u32::from_le_bytes([b[8], b[9], b[10], b[11]]),
            status: i32::from_le_bytes([b[12], b[13], b[14], b[15]]),
        }
    }
}

const _: () = {
    assert!(size_of::<FrameHeader>() == 16);
    assert!(offset_of!(FrameHeader, len) == 0);
    assert!(offset_of!(FrameHeader, opcode) == 4);
    assert!(offset_of!(FrameHeader, flags) == 6);
    assert!(offset_of!(FrameHeader, request_id) == 8);
    assert!(offset_of!(FrameHeader, status) == 12);
};
