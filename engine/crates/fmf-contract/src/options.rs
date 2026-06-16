//! Wire enumerations of `FmfQueryOptions` and `FmfVolumeStatus.state`
//! (docs/ARCHITECTURE.md opcode table).
//!
//! These are the canonical values; fmf-core uses these enums directly (no
//! wire↔engine mapping layer).

/// Which result column results are ordered by (`FmfQueryOptions.sort_key`).
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortKey {
    /// Sort by file name.
    #[default]
    Name = 0,
    /// Sort by file size in bytes.
    Size = 1,
    /// Sort by last-modified time.
    Mtime = 2,
}

/// How the query is matched against names (`FmfQueryOptions.case_mode`).
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CaseMode {
    /// Insensitive unless the query contains an uppercase letter.
    #[default]
    Smart = 0,
    /// Case-insensitive matching.
    Insensitive = 1,
    /// Case-sensitive matching.
    Sensitive = 2,
}

/// Lifecycle state of a volume's index (`FmfVolumeStatus.state`).
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeState {
    /// Initial full scan in progress; the index is not yet complete.
    Scanning = 0,
    /// Index is complete and serving queries.
    Ready = 1,
    /// A full re-scan is in progress while the prior index keeps serving.
    Rescanning = 2,
    /// The volume could not be indexed.
    Failed = 3,
}

/// Which haystack a whole-query regex runs against
/// (`FmfQueryOptions.regex_mode` bit1; ADR-0023).
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RegexScope {
    /// Match against the file name only.
    #[default]
    Name = 0,
    /// Match against the full path.
    Path = 1,
}

// Wire u32 → enum, defaulting unknown values like the boundaries always
// did (pure value conversion — the one place the mapping table lives).
impl SortKey {
    /// Decode a wire `u32` into a `SortKey`, defaulting unknown values to `Name`.
    #[must_use]
    pub const fn from_u32(v: u32) -> Self {
        match v {
            1 => Self::Size,
            2 => Self::Mtime,
            _ => Self::Name,
        }
    }
}

impl CaseMode {
    /// Decode a wire `u32` into a `CaseMode`, defaulting unknown values to `Smart`.
    #[must_use]
    pub const fn from_u32(v: u32) -> Self {
        match v {
            1 => Self::Insensitive,
            2 => Self::Sensitive,
            _ => Self::Smart,
        }
    }
}

impl RegexScope {
    /// Decode a wire `u32` into a `RegexScope`, defaulting unknown values to `Name`.
    #[must_use]
    pub const fn from_u32(v: u32) -> Self {
        match v {
            1 => Self::Path,
            _ => Self::Name,
        }
    }
}
