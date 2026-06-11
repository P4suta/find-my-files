//! Wire enumerations of `FmfQueryOptions` and `FmfVolumeStatus.state`
//! (docs/ARCHITECTURE.md オペコード表). These are the canonical values;
//! fmf-core uses these enums directly (no wire↔engine mapping layer).

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortKey {
    #[default]
    Name = 0,
    Size = 1,
    Mtime = 2,
}

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CaseMode {
    /// Insensitive unless the query contains an uppercase letter.
    #[default]
    Smart = 0,
    Insensitive = 1,
    Sensitive = 2,
}

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeState {
    Scanning = 0,
    Ready = 1,
    Rescanning = 2,
    Failed = 3,
}

// Wire u32 → enum, defaulting unknown values like the boundaries always
// did (pure value conversion — the one place the mapping table lives).
impl SortKey {
    pub const fn from_u32(v: u32) -> Self {
        match v {
            1 => Self::Size,
            2 => Self::Mtime,
            _ => Self::Name,
        }
    }
}

impl CaseMode {
    pub const fn from_u32(v: u32) -> Self {
        match v {
            1 => Self::Insensitive,
            2 => Self::Sensitive,
            _ => Self::Smart,
        }
    }
}
