//! `stats` — per-column memory accounting (the B/entry RAM gate figure) plus
//! the read-only measurement inputs for pool/column layout decisions.

use fmf_core::index::VolumeIndex;
use fmf_core::query;

use super::build_index;
use super::ctx::Ctx;

pub fn stats(
    drive: &str,
    trigram_estimate: bool,
    name_stats: bool,
    dict_estimate: bool,
    ctx: Ctx,
) -> Result<(), Box<dyn std::error::Error>> {
    let idx = build_index(drive, ctx)?;
    // Mirror the engine's Ready state (offset table prewarmed) so the
    // accounting reflects what the app actually holds.
    query::prewarm(&idx);
    let mut s = idx.stats(drive);
    s.add_derived_bytes(query::derived_cache_bytes(&idx));
    // The B/file RAM gate reads the steady working set, not the scan peak.
    let ws = fmf_core::mft::current_working_set();
    let ws_per_entry = if idx.is_empty() {
        0.0
    } else {
        ws as f64 / idx.len() as f64
    };

    // `--format json`: one combined, format_version-stamped document on stdout
    // (the opt-in estimates merge in as fields) — not the several separate JSON
    // blobs the human dump prints.
    if ctx.is_json() {
        let mut doc = serde_json::Map::new();
        doc.insert("columns".to_owned(), serde_json::to_value(&s)?);
        doc.insert("working_set_bytes".to_owned(), ws.into());
        doc.insert(
            "working_set_bytes_per_entry".to_owned(),
            ws_per_entry.into(),
        );
        if trigram_estimate {
            doc.insert(
                "trigram_estimate".to_owned(),
                serde_json::to_value(compute_trigram_estimate(&idx))?,
            );
        }
        if name_stats {
            doc.insert(
                "name_stats".to_owned(),
                serde_json::to_value(compute_name_stats(&idx))?,
            );
        }
        if dict_estimate {
            doc.insert(
                "dict_estimate".to_owned(),
                serde_json::to_value(compute_dict_estimate(&idx))?,
            );
            doc.insert(
                "orig_estimate".to_owned(),
                serde_json::to_value(compute_orig_estimate(&idx))?,
            );
        }
        return super::json::emit(&serde_json::Value::Object(doc));
    }

    // Human: the per-column accounting dump plus the decorative working-set line.
    println!("{}", serde_json::to_string_pretty(&s)?);
    eprintln!(
        "current working set {:.1} MiB (≈{ws_per_entry:.0} B/entry — the RAM gate figure)",
        ws as f64 / (1024.0 * 1024.0),
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
    if dict_estimate {
        print_dict_estimate(&idx);
        print_orig_estimate(&idx);
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
    /// Sizes that cannot live in a u32 column (≥ `u32::MAX`, sentinel
    /// included) — go/no-go input for the `size_lo+overflow` layout.
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
        if idx.size(id) >= u64::from(u32::MAX) {
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
        predicted_savings_sorted_pairs: 8.0f64.mul_add(-(1.0 - f), f * mean),
    }
}

/// A byte-trigram index estimate over the live folded names (criterion (2) of
/// the n-gram go/no-go in ARCHITECTURE.md). Read-only; nothing is built.
#[derive(serde::Serialize)]
struct TrigramEstimate {
    distinct: u64,
    postings: u64,
    total_bytes: u64,
    per_entry: f64,
}

/// Estimate a byte-trigram index over the live folded names: distinct
/// trigrams (dictionary) + total postings (delta-varint assumed ~1.5B each).
fn compute_trigram_estimate(idx: &VolumeIndex) -> TrigramEstimate {
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
    TrigramEstimate {
        distinct: distinct.len() as u64,
        postings,
        total_bytes: total,
        per_entry: if live > 0 {
            total as f64 / live as f64
        } else {
            0.0
        },
    }
}

fn print_trigram_estimate(idx: &VolumeIndex) {
    let e = compute_trigram_estimate(idx);
    println!(
        "trigram estimate: {} distinct, {} postings → ≈{:.1} MiB ({:.1} B/entry; go/no-go gate: ≤15 B/entry AND total bytes/entry ≤110)",
        e.distinct,
        e.postings,
        e.total_bytes as f64 / (1024.0 * 1024.0),
        e.per_entry
    );
}

/// The projected name-dictionary-encoding delta (Phase-2 go/no-go input).
/// `--trigram-estimate`'s sibling: read-only, nothing is built.
#[derive(serde::Serialize)]
struct DictEstimate {
    /// Distinct folded names = dictionary entry count (D).
    distinct: u64,
    /// Σ length over the distinct folded names = dictionary pool bytes (`B_d`).
    dict_bytes: u64,
    /// Σ folded length over every live entry = today's `lower_pool` content.
    folded_logical: u64,
    live: u64,
    /// Mean distinct-name length `B_d/D` — the figure the net is most
    /// sensitive to (short high-count duplicates pull it below the mean).
    mean_distinct_len: f64,
    /// Net B/entry at rest (the dedup interner freed after build/compaction).
    net_at_rest: f64,
    /// Net B/entry if that interner were kept resident — the variant the
    /// design rejects (it erases the win); shown for contrast.
    net_resident: f64,
}

/// Per-distinct cost assumed for the *resident* interner: a folded→id
/// hashbrown table (u32 id + control byte, ~0.875 load, key referenced from
/// the pool) amortises to ~16 B/distinct.
const RESIDENT_INTERNER_BYTES_PER_DISTINCT: f64 = 16.0;

/// Project the memory delta of storing each distinct folded name once in a
/// dictionary and replacing the per-entry `name_off`+`name_len` (6 B) with a
/// single `name_id` (4 B). `orig_off`/`orig_pool` stay per-entry — a shared
/// folded name can back differing originals (README/readme), so the original
/// columns cannot dedup (ADR-0004). The whole saving is the deduped pool.
fn compute_dict_estimate(idx: &VolumeIndex) -> DictEstimate {
    use std::collections::HashSet;
    let mut distinct: HashSet<&[u8]> = HashSet::new();
    let mut folded_logical = 0u64;
    let mut live = 0u64;
    for id in 0..idx.len() as u32 {
        if !idx.is_live(id) {
            continue;
        }
        live += 1;
        let folded = idx.lower_name(id);
        folded_logical += folded.len() as u64;
        distinct.insert(folded);
    }
    let d = distinct.len() as u64;
    let dict_bytes: u64 = distinct.iter().map(|s| s.len() as u64).sum();
    let d_f = d as f64;
    let n = live.max(1) as f64;
    // Per-entry net = pool dedup + column swap + dictionary metadata, all /N:
    //   pool:      B_d − folded_logical            (the dedup win, ≤ 0)
    //   columns:   −name_len(2)  (name_off→name_id is 4→4)  = −2·N
    //   dict meta: +dict_off(4·D) +dict_len(2·D)            = +6·D
    //   orig_off / orig_pool: unchanged (stay per-entry)
    // This is the Phase-2 projection. Lever 2 (ADR-0033) later dropped the
    // `dict_len` column (lengths derive from the gapless `dict_off`), so the
    // realized directory cost is +4·D — a further −2 B/entry over the figure
    // printed here.
    let pool_delta = dict_bytes as f64 - folded_logical as f64;
    let two_n = 2.0 * n;
    let net_at_rest = 6.0f64.mul_add(d_f, pool_delta - two_n) / n;
    let net_resident = net_at_rest + RESIDENT_INTERNER_BYTES_PER_DISTINCT * d_f / n;
    DictEstimate {
        distinct: d,
        dict_bytes,
        folded_logical,
        live,
        mean_distinct_len: if d > 0 { dict_bytes as f64 / d_f } else { 0.0 },
        net_at_rest,
        net_resident,
    }
}

fn print_dict_estimate(idx: &VolumeIndex) {
    let e = compute_dict_estimate(idx);
    let n = e.live.max(1) as f64;
    println!(
        "dict estimate (folded — realized in Phase 2/ADR-0032): {} distinct folded names, {} dict bytes (L_d={:.1} B; folded pool {:.1} B/entry) → would-be net {:+.1} B/entry at rest ({:+.1} resident)",
        e.distinct,
        e.dict_bytes,
        e.mean_distinct_len,
        e.folded_logical as f64 / n,
        e.net_at_rest,
        e.net_resident,
    );
}

/// The delta of interning the original-spelling pool (ADR-0033 Lever 1,
/// realized): the entries whose original differs from the fold store their
/// original verbatim in `orig_pool`, and those originals duplicate heavily
/// across a volume. Read-only.
#[derive(serde::Serialize)]
struct OrigEstimate {
    /// Entries whose original differs from the fold (own an `orig_pool` copy).
    differing: u64,
    /// Distinct originals among them = deduped `orig_pool` entry count.
    distinct: u64,
    /// Σ length over the distinct originals = deduped `orig_pool` bytes.
    dict_bytes: u64,
    /// Σ original length over the differing entries = today's `orig_pool`.
    orig_logical: u64,
    live: u64,
    /// Net B/entry of interning: the pool shrinks to the distinct originals and
    /// `orig_off` stays a 4-byte offset into it — no length table, since the
    /// fold is length-preserving (ADR-0004), so the whole win is the pool.
    net: f64,
}

fn compute_orig_estimate(idx: &VolumeIndex) -> OrigEstimate {
    use std::collections::HashSet;
    let mut distinct: HashSet<&[u8]> = HashSet::new();
    let mut orig_logical = 0u64;
    let mut differing = 0u64;
    let mut live = 0u64;
    for id in 0..idx.len() as u32 {
        if !idx.is_live(id) {
            continue;
        }
        live += 1;
        let name = idx.name(id);
        if name != idx.lower_name(id) {
            differing += 1;
            orig_logical += name.len() as u64;
            distinct.insert(name);
        }
    }
    let d = distinct.len() as u64;
    let dict_bytes: u64 = distinct.iter().map(|s| s.len() as u64).sum();
    let n = live.max(1) as f64;
    // Table-free: orig_off keeps pointing into the (now deduped) pool and the
    // length comes from the fold (ADR-0004), so the only delta is the pool.
    let net = (dict_bytes as f64 - orig_logical as f64) / n;
    OrigEstimate {
        differing,
        distinct: d,
        dict_bytes,
        orig_logical,
        live,
        net,
    }
}

fn print_orig_estimate(idx: &VolumeIndex) {
    let e = compute_orig_estimate(idx);
    let n = e.live.max(1) as f64;
    println!(
        "orig estimate (Lever 1 — realized in ADR-0033): {} differing entries, {} distinct originals, {} deduped bytes (orig pool today {:.1} B/entry) → net {:+.1} B/entry (length from the fold, no offset table)",
        e.differing,
        e.distinct,
        e.dict_bytes,
        e.orig_logical as f64 / n,
        e.net,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmf_core::index::{Frn, RawEntry, VolumeIndexBuilder};

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
                parent_frn: Frn(5),
                frn: Frn(100 + i as u64),
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
            parent_frn: Frn(5),
            frn: Frn(200),
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
        assert_eq!(s.fold_identical, 3); // readme.txt x2 + the Japanese-named fixture
        assert_eq!(s.size_ge_4gib, 1);
        assert!((s.unique_name_ratio - 5.0 / 6.0).abs() < 1e-9);
        assert!((s.unique_folded_ratio - 5.0 / 6.0).abs() < 1e-9);
        // Lengths in WTF-8 bytes: 2, 10, 10, 8, 13, 4 → mean 47/6, max 13.
        assert!((s.name_len.mean - 47.0 / 6.0).abs() < 1e-9);
        assert_eq!(s.name_len.max, 13);
    }

    #[test]
    fn dict_estimate_projects_dedup_savings() {
        let mut b = VolumeIndexBuilder::new("C:", 5);
        // Three identical folded names + one differing-case name. Root "C:"
        // folds to "c:" (len 2) and is its own distinct entry.
        let names: &[&str] = &["report.log", "report.log", "report.log", "BIG_FILE.DAT"];
        for (i, name) in names.iter().enumerate() {
            let units: Vec<u16> = name.encode_utf16().collect();
            b.push(RawEntry {
                parent_frn: Frn(5),
                frn: Frn(100 + i as u64),
                name_utf16: &units,
                is_dir: false,
                is_reparse: false,
                is_hidden: false,
                is_system: false,
                size: 0,
                mtime: 0,
            });
        }
        let idx = b.finish();

        let e = compute_dict_estimate(&idx);
        // live = root "c:" + 4 pushed; distinct folded = {c:, report.log, big_file.dat}.
        assert_eq!(e.live, 5);
        assert_eq!(e.distinct, 3);
        // dict bytes = 2 ("c:") + 10 ("report.log") + 12 ("big_file.dat") = 24.
        assert_eq!(e.dict_bytes, 24);
        // folded logical = 2 + 10*3 + 12 = 44.
        assert_eq!(e.folded_logical, 44);
        assert!((e.mean_distinct_len - 8.0).abs() < 1e-9);
        // net = (24 − 44 − 2*5 + 6*3) / 5 = −12/5 = −2.4 B/entry.
        assert!((e.net_at_rest - (-2.4)).abs() < 1e-9);
        // resident interner adds 16*3/5 = 9.6 → +7.2.
        assert!((e.net_resident - 7.2).abs() < 1e-9);
    }

    #[test]
    fn orig_estimate_projects_original_dedup() {
        let mut b = VolumeIndexBuilder::new("C:", 5);
        // Four "README" + one "Makefile": all differ from their fold, and the
        // duplicated originals dedup. Root "C:" differs too ("c:").
        let names: &[&str] = &["README", "README", "README", "README", "Makefile"];
        for (i, name) in names.iter().enumerate() {
            let units: Vec<u16> = name.encode_utf16().collect();
            b.push(RawEntry {
                parent_frn: Frn(5),
                frn: Frn(100 + i as u64),
                name_utf16: &units,
                is_dir: false,
                is_reparse: false,
                is_hidden: false,
                is_system: false,
                size: 0,
                mtime: 0,
            });
        }
        let idx = b.finish();

        let e = compute_orig_estimate(&idx);
        // live = root "C:" + 5 pushed = 6; all differ (uppercase present).
        assert_eq!(e.live, 6);
        assert_eq!(e.differing, 6);
        // distinct originals = {C:, README, Makefile} = 3.
        assert_eq!(e.distinct, 3);
        // dict bytes = 2 ("C:") + 6 ("README") + 8 ("Makefile") = 16.
        assert_eq!(e.dict_bytes, 16);
        // orig logical = 2 + 6*4 + 8 = 34.
        assert_eq!(e.orig_logical, 34);
        // net = (16 − 34) / 6 = −18/6 = −3.0 (table-free: length from the fold).
        assert!((e.net - (-3.0)).abs() < 1e-9);
    }
}
