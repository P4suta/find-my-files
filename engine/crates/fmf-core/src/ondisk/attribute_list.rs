//! Checked extent geometry and continuation-discovery work.

/// Checked logical byte geometry for one non-resident extent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExtentLayout {
    lowest_vcn: u64,
    clusters: u64,
    logical_base: u64,
    logical_end: u64,
}

impl ExtentLayout {
    /// Reject invalid VCNs, cluster geometry, overflow, or an end beyond the volume.
    #[must_use]
    pub fn new(lowest: i64, highest: i64, cluster_size: u64, volume_size: u64) -> Option<Self> {
        if !cluster_size.is_power_of_two() {
            return None;
        }
        let lowest_vcn = u64::try_from(lowest).ok()?;
        let highest_vcn = u64::try_from(highest).ok()?;
        let clusters = highest_vcn.checked_sub(lowest_vcn)?.checked_add(1)?;
        let logical_base = lowest_vcn.checked_mul(cluster_size)?;
        let logical_end = highest_vcn.checked_add(1)?.checked_mul(cluster_size)?;
        if logical_end > volume_size {
            return None;
        }
        Some(Self {
            lowest_vcn,
            clusters,
            logical_base,
            logical_end,
        })
    }

    /// First virtual cluster number.
    #[must_use]
    pub const fn lowest_vcn(self) -> u64 {
        self.lowest_vcn
    }

    /// Positive number of clusters in the extent.
    #[must_use]
    pub const fn clusters(self) -> u64 {
        self.clusters
    }

    /// First byte offset within the logical stream.
    #[must_use]
    pub const fn logical_base(self) -> u64 {
        self.logical_base
    }

    /// Exclusive byte endpoint within the logical stream.
    #[must_use]
    pub const fn logical_end(self) -> u64 {
        self.logical_end
    }
}

/// Independent caps for continuation discovery.
#[derive(Clone, Copy)]
pub struct ClosureLimits {
    /// Maximum discovery callbacks.
    pub passes: usize,
    /// Maximum cumulative prefix bytes.
    pub scanned_bytes: u64,
    /// Maximum entry count per callback and distinct target identities.
    pub entries: usize,
}

/// Cumulative admitted work before another discovery callback.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClosureWork {
    passes: usize,
    scanned_bytes: u64,
}

impl ClosureWork {
    /// Charge one callback and its prefix bytes, or reject without updating the work.
    #[must_use]
    pub fn advance(self, prefix: u64, limits: ClosureLimits) -> Option<Self> {
        let passes = self.passes.checked_add(1)?;
        let scanned_bytes = self.scanned_bytes.checked_add(prefix)?;
        if passes > limits.passes || scanned_bytes > limits.scanned_bytes {
            return None;
        }
        Some(Self {
            passes,
            scanned_bytes,
        })
    }
}

#[cfg(kani)]
#[path = "attribute_list_proofs.rs"]
mod kani_proofs;
