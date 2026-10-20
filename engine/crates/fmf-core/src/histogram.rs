//! Bounded logarithmic buckets for runtime latency measurements.

/// Number of buckets; the final bucket includes every larger sample.
pub const BUCKET_COUNT: usize = 32;

/// An index into a fixed latency histogram.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BucketIndex(usize);

impl BucketIndex {
    /// Clamp an index to the final overflow bucket.
    #[must_use]
    pub const fn from_slot_clamped(slot: usize) -> Self {
        Self(if slot < BUCKET_COUNT {
            slot
        } else {
            BUCKET_COUNT - 1
        })
    }

    /// Select the logarithmic bucket for a microsecond measurement.
    #[must_use]
    pub const fn for_micros(micros: u64) -> Self {
        let nonzero = if micros == 0 { 1 } else { micros };
        Self::from_slot_clamped((u64::BITS - nonzero.leading_zeros() - 1) as usize)
    }

    /// Read the checked array index.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0
    }

    /// Return the bucket boundary, using the observed maximum for the overflow bucket.
    #[must_use]
    pub const fn upper_bound(self, maximum: u64) -> u64 {
        if self.0 == BUCKET_COUNT - 1 {
            maximum
        } else {
            1_u64 << (self.0 + 1)
        }
    }
}

#[cfg(kani)]
#[path = "histogram_proofs.rs"]
mod kani_proofs;
