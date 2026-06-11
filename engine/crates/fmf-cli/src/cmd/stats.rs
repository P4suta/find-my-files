//! `stats` — per-column memory accounting (the B/entry RAM gate figure) plus
//! the read-only measurement inputs for pool/column layout decisions.

use fmf_core::index::VolumeIndex;
use fmf_core::query;

use super::build_index;

pub fn stats(
    drive: &str,
    trigram_estimate: bool,
    name_stats: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let idx = build_index(drive)?;
    // Mirror the engine's Ready state (offset table prewarmed) so the
    // accounting reflects what the app actually holds.
    query::prewarm(&idx);
    let mut s = idx.stats(drive);
    s.add_derived_bytes(query::derived_cache_bytes(&idx));
    println!("{}", serde_json::to_string_pretty(&s)?);
    // The B/file RAM gate reads the steady working set, not the scan peak.
    let ws = fmf_core::mft::current_working_set();
    eprintln!(
        "current working set {:.1} MiB (≈{:.0} B/entry — the RAM gate figure)",
        ws as f64 / (1024.0 * 1024.0),
        if idx.is_empty() {
            0.0
        } else {
            ws as f64 / idx.len() as f64
        }
    );
    if trigram_estimate {
        print_trigram_estimate(&idx);
    }
    if name_stats {
        println!(
            "{}",
            serde_json::to_string_pretty(&compute_name_stats(&idx))?
        );
    }
    Ok(())
}

/// Per-name statistics over the live entries — the measured inputs for
/// pool/column layout decisions (orig-overflow schemes, size column width).
#[derive(serde::Serialize)]
struct NameLenStats {
    mean: f64,
    p50: u16,
    p90: u16,
    p99: u16,
    max: u16,
}

#[derive(serde::Serialize)]
struct NameStats {
    live: u64,
    /// Entries whose folded form equals the original byte-for-byte — these
    /// need no second copy under an orig-overflow pool layout.
    fold_identical: u64,
    fold_identical_ratio: f64,
    unique_name_ratio: f64,
    unique_folded_ratio: f64,
    /// WTF-8 byte lengths (identical for both pools by the fold rule).
    name_len: NameLenStats,
    /// Sizes that cannot live in a u32 column (≥ u32::MAX, sentinel
    /// included) — go/no-go input for the size_lo+overflow layout.
    size_ge_4gib: u64,
    size_ge_4gib_ratio: f64,
    /// Projected B/entry savings of dropping the original-name pool for
    /// fold-identical entries, per overflow-reference scheme.
    predicted_savings_full_column: f64,
    predicted_savings_sorted_pairs: f64,
}

fn compute_name_stats(idx: &VolumeIndex) -> NameStats {
    use std::collections::HashSet;
    let mut live = 0u64;
    let mut fold_identical = 0u64;
    let mut len_sum = 0u64;
    let mut lens: Vec<u16> = Vec::with_capacity(idx.len());
    let mut uniq: HashSet<&[u8]> = HashSet::new();
    let mut uniq_folded: HashSet<&[u8]> = HashSet::new();
    let mut size_ge_4gib = 0u64;
    for id in 0..idx.len() as u32 {
        if !idx.is_live(id) {
            continue;
        }
        live += 1;
        let name = idx.name(id);
        if name == idx.lower_name(id) {
            fold_identical += 1;
        }
        len_sum += name.len() as u64;
        lens.push(name.len() as u16);
        uniq.insert(name);
        uniq_folded.insert(idx.lower_name(id));
        if idx.size(id) >= u32::MAX as u64 {
            size_ge_4gib += 1;
        }
    }
    lens.sort_unstable();
    let pct = |p: f64| {
        if lens.is_empty() {
            0
        } else {
            lens[((lens.len() - 1) as f64 * p) as usize]
        }
    };
    let n = live.max(1) as f64;
    let f = fold_identical as f64 / n;
    let mean = len_sum as f64 / n;
    NameStats {
        live,
        fold_identical,
        fold_identical_ratio: f,
        unique_name_ratio: uniq.len() as f64 / n,
        unique_folded_ratio: uniq_folded.len() as f64 / n,
        name_len: NameLenStats {
            mean,
            p50: pct(0.50),
            p90: pct(0.90),
            p99: pct(0.99),
            max: lens.last().copied().unwrap_or(0),
        },
        size_ge_4gib,
        size_ge_4gib_ratio: size_ge_4gib as f64 / n,
        // scheme (ii): orig_off u32 column for every entry.
        predicted_savings_full_column: f * mean - 4.0,
        // scheme (i): sorted (id, off) pairs for differing entries only.
        predicted_savings_sorted_pairs: f * mean - 8.0 * (1.0 - f),
    }
}

/// Estimate a byte-trigram index over the live folded names: distinct
/// trigrams (dictionary) + total postings (delta-varint assumed ~1.5B
/// each). Feeds criterion (2) of the n-gram go/no-go in ARCHITECTURE.md.
fn print_trigram_estimate(idx: &VolumeIndex) {
    let mut distinct: std::collections::HashSet<[u8; 3]> = std::collections::HashSet::new();
    let mut postings = 0u64;
    let mut live = 0u64;
    for id in 0..idx.len() as u32 {
        if !idx.is_live(id) {
            continue;
        }
        live += 1;
        let name = idx.lower_name(id);
        for w in name.windows(3) {
            distinct.insert([w[0], w[1], w[2]]);
            postings += 1;
        }
    }
    let dict_bytes = distinct.len() as u64 * (3 + 4 + 4); // key + offset + len
    let posting_bytes = postings * 3 / 2; // delta varint ≈ 1.5B/posting
    let total = dict_bytes + posting_bytes;
    let per_entry = if live > 0 {
        total as f64 / live as f64
    } else {
        0.0
    };
    println!(
        "trigram estimate: {} distinct, {} postings → ≈{:.1} MiB ({:.1} B/entry; go/no-go gate: ≤15 B/entry AND total bytes/entry ≤110)",
        distinct.len(),
        postings,
        total as f64 / (1024.0 * 1024.0),
        per_entry
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmf_core::index::{RawEntry, VolumeIndexBuilder};

    #[test]
    fn name_stats_counts_fold_dup_lengths_and_big_sizes() {
        let mut b = VolumeIndexBuilder::new("C:", 5);
        // (name, size): two duplicates, one cased, one Japanese, plus a
        // lone-surrogate name pushed below. Root "C:" folds to "c:" and
        // counts as a differing entry of length 2.
        let entries: &[(&str, u64)] = &[
            ("readme.txt", 10),
            ("readme.txt", 20),
            ("File.TXT", 1 << 33), // differs + ≥4GiB
            ("日本語.txt", 30),
        ];
        for (i, (name, size)) in entries.iter().enumerate() {
            let units: Vec<u16> = name.encode_utf16().collect();
            b.push(RawEntry {
                record: 100 + i as u64,
                parent_record: 5,
                frn: 100 + i as u64,
                name_utf16: &units,
                is_dir: false,
                is_reparse: false,
                is_hidden: false,
                is_system: false,
                size: *size,
                mtime: 0,
            });
        }
        // "A" + lone high surrogate: legal NTFS, must count as differing
        // (the 'A' folds) without tripping the WTF-8 handling.
        b.push(RawEntry {
            record: 200,
            parent_record: 5,
            frn: 200,
            name_utf16: &[0x0041, 0xD800],
            is_dir: false,
            is_reparse: false,
            is_hidden: false,
            is_system: false,
            size: 0,
            mtime: 0,
        });
        let idx = b.finish();

        let s = compute_name_stats(&idx);
        assert_eq!(s.live, 6);
        assert_eq!(s.fold_identical, 3); // readme.txt ×2 + 日本語.txt
        assert_eq!(s.size_ge_4gib, 1);
        assert!((s.unique_name_ratio - 5.0 / 6.0).abs() < 1e-9);
        assert!((s.unique_folded_ratio - 5.0 / 6.0).abs() < 1e-9);
        // Lengths in WTF-8 bytes: 2, 10, 10, 8, 13, 4 → mean 47/6, max 13.
        assert!((s.name_len.mean - 47.0 / 6.0).abs() < 1e-9);
        assert_eq!(s.name_len.max, 13);
    }
}
