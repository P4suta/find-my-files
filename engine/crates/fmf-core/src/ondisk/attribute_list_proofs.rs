use super::{ClosureLimits, ClosureWork, ExtentLayout};

fn original_geometry(
    lowest: i64,
    highest: i64,
    cluster_size: u64,
    volume_size: u64,
) -> Option<(u64, u64, u64, u64)> {
    if cluster_size == 0 || volume_size == 0 || !cluster_size.is_power_of_two() {
        return None;
    }
    let lowest = u64::try_from(lowest).ok()?;
    let highest = u64::try_from(highest).ok()?;
    let clusters = highest.checked_sub(lowest)?.checked_add(1)?;
    let base = lowest.checked_mul(cluster_size)?;
    let end = highest.checked_add(1)?.checked_mul(cluster_size)?;
    if end > volume_size {
        return None;
    }
    Some((lowest, clusters, base, end))
}

#[kani::proof]
#[kani::unwind(1)]
#[kani::solver(cadical)]
fn checked_extent_geometry_preserves_every_scalar_admission_boundary() {
    let lowest: i64 = kani::any();
    let highest: i64 = kani::any();
    let cluster_size: u64 = kani::any();
    let volume_size: u64 = kani::any();
    let actual = ExtentLayout::new(lowest, highest, cluster_size, volume_size);
    assert_eq!(
        actual.map(|layout| (
            layout.lowest_vcn(),
            layout.clusters(),
            layout.logical_base(),
            layout.logical_end()
        )),
        original_geometry(lowest, highest, cluster_size, volume_size)
    );
    if let Some(layout) = actual {
        assert!(cluster_size.is_power_of_two());
        assert!(layout.clusters() > 0);
        assert!(layout.logical_base() < layout.logical_end());
        assert!(layout.logical_end() <= volume_size);
    }
    kani::cover!(actual.is_some() && lowest == 0);
    kani::cover!(actual.is_some() && lowest > 0);
    kani::cover!(actual.is_none() && cluster_size == 0);
    kani::cover!(actual.is_none() && volume_size == 0);
}

#[kani::proof]
#[kani::unwind(1)]
fn discovery_work_admits_exact_caps_and_rejects_overflow_without_changing_state() {
    let work = ClosureWork {
        passes: kani::any(),
        scanned_bytes: kani::any(),
    };
    let limits = ClosureLimits {
        passes: kani::any(),
        scanned_bytes: kani::any(),
        entries: kani::any(),
    };
    let prefix: u64 = kani::any();
    let actual = work.advance(prefix, limits);
    let expected = work
        .passes
        .checked_add(1)
        .zip(work.scanned_bytes.checked_add(prefix))
        .filter(|&(passes, bytes)| passes <= limits.passes && bytes <= limits.scanned_bytes);
    assert_eq!(
        actual.map(|next| (next.passes, next.scanned_bytes)),
        expected
    );
    if let Some(next) = actual {
        assert!(next.passes > work.passes);
        assert!(next.scanned_bytes >= work.scanned_bytes);
        assert!(next.passes <= limits.passes);
        assert!(next.scanned_bytes <= limits.scanned_bytes);
    }
    kani::cover!(actual.is_some_and(|next| next.passes == limits.passes));
    kani::cover!(actual.is_some_and(|next| next.scanned_bytes == limits.scanned_bytes));
    kani::cover!(actual.is_none() && work.passes == usize::MAX);
    kani::cover!(actual.is_none() && work.scanned_bytes == u64::MAX && prefix > 0);
}
