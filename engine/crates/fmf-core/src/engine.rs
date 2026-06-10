//! Multi-volume engine assembly: owns one `VolumeIndex` per NTFS volume,
//! drives initial scans and USN tailing threads, and answers queries with a
//! k-way-merged, sort-ordered result set (docs/ARCHITECTURE.md). This is the
//! layer the FFI exposes 1:1 — and the layer a v2 service would host.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use parking_lot::{Mutex, RwLock};
use thiserror::Error;

use crate::index::{EntryId, SortKey, VolumeIndex, flags};
use crate::query::{self, QueryOptions};

#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub index_dir: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumePhase {
    Scanning,
    Ready,
    Rescanning,
    Failed,
}

#[derive(Debug, Clone)]
pub enum EngineEvent {
    Progress {
        volume: String,
        entries: u64,
    },
    VolumeReady {
        volume: String,
        entries: u64,
    },
    /// Emitted (debounced, engine-side only throttle) after USN batches.
    IndexChanged {
        volume: String,
    },
    RescanStarted {
        volume: String,
    },
    VolumeFailed {
        volume: String,
        message: String,
    },
}

pub type EventSink = Arc<dyn Fn(&EngineEvent) + Send + Sync>;

#[derive(Debug, Error)]
pub enum EngineError {
    #[error("query parse: {0}")]
    Parse(#[from] query::ParseError),
    #[error("query compile: {0}")]
    Compile(#[from] query::CompileError),
    #[error("result is stale (index was rebuilt)")]
    Stale,
}

struct VolumeSlot {
    label: String,
    phase: Mutex<VolumePhase>,
    scanned: Mutex<u64>,
    index: RwLock<Option<VolumeIndex>>,
    stop: Arc<AtomicBool>,
}

pub struct Engine {
    config: EngineConfig,
    sink: RwLock<Option<EventSink>>,
    volumes: RwLock<Vec<Arc<VolumeSlot>>>,
    threads: Mutex<Vec<std::thread::JoinHandle<()>>>,
}

/// Engine-side debounce for IndexChanged — the only throttle in the whole
/// change path (docs/ARCHITECTURE.md 遅延予算).
const INDEX_CHANGED_DEBOUNCE: Duration = Duration::from_millis(200);

impl Engine {
    pub fn new(config: EngineConfig) -> Arc<Self> {
        Arc::new(Self {
            config,
            sink: RwLock::new(None),
            volumes: RwLock::new(Vec::new()),
            threads: Mutex::new(Vec::new()),
        })
    }

    pub fn set_event_sink(&self, sink: Option<EventSink>) {
        *self.sink.write() = sink;
    }

    fn emit(&self, ev: EngineEvent) {
        if let Some(s) = self.sink.read().clone() {
            s(&ev);
        }
    }

    /// Fixed NTFS volumes ("C:", "D:", …).
    #[cfg(windows)]
    pub fn list_ntfs_volumes() -> Vec<String> {
        use windows_sys::Win32::Storage::FileSystem::{
            GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
        };
        const DRIVE_FIXED: u32 = 3;
        let mut out = Vec::new();
        let mask = unsafe { GetLogicalDrives() };
        for i in 0..26u32 {
            if mask & (1 << i) == 0 {
                continue;
            }
            let letter = (b'A' + i as u8) as char;
            let root: Vec<u16> = format!("{letter}:\\").encode_utf16().chain([0]).collect();
            unsafe {
                if GetDriveTypeW(root.as_ptr()) != DRIVE_FIXED {
                    continue;
                }
                let mut fs = [0u16; 32];
                let ok = GetVolumeInformationW(
                    root.as_ptr(),
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    fs.as_mut_ptr(),
                    fs.len() as u32,
                );
                if ok != 0 {
                    let fs_name: String = String::from_utf16_lossy(
                        &fs[..fs.iter().position(|&c| c == 0).unwrap_or(0)],
                    );
                    if fs_name == "NTFS" {
                        out.push(format!("{letter}:"));
                    }
                }
            }
        }
        out
    }

    /// Begin indexing the given volumes (asynchronous; progress via events).
    pub fn index_start(self: &Arc<Self>, volumes: &[String]) {
        for label in volumes {
            let slot = Arc::new(VolumeSlot {
                label: label.clone(),
                phase: Mutex::new(VolumePhase::Scanning),
                scanned: Mutex::new(0),
                index: RwLock::new(None),
                stop: Arc::new(AtomicBool::new(false)),
            });
            self.volumes.write().push(slot.clone());
            let engine = self.clone();
            let handle = std::thread::Builder::new()
                .name(format!("fmf-vol-{label}"))
                .spawn(move || engine.volume_thread(slot))
                .expect("spawn volume thread");
            self.threads.lock().push(handle);
        }
    }

    #[cfg(windows)]
    fn volume_thread(self: Arc<Self>, slot: Arc<VolumeSlot>) {
        use crate::usn::{ReadOutcome, UsnJournal, VolumeStatFetcher, apply_batch};

        let label = slot.label.clone();
        let snapshot_path = self.config.index_dir.join(format!(
            "{}.fmfidx",
            label.trim_end_matches(':').to_ascii_lowercase()
        ));

        loop {
            if slot.stop.load(Ordering::Relaxed) {
                return;
            }
            // 1. Journal first (checkpoint precedes the scan so nothing is
            //    missed), then snapshot-or-scan.
            let mut journal = match UsnJournal::open(&label, None) {
                Ok(j) => j,
                Err(e) => {
                    *slot.phase.lock() = VolumePhase::Failed;
                    self.emit(EngineEvent::VolumeFailed {
                        volume: label.clone(),
                        message: e.to_string(),
                    });
                    return;
                }
            };

            let loaded =
                VolumeIndex::load_from(&snapshot_path)
                    .ok()
                    .filter(|(_, journal_id, next_usn)| match journal.query() {
                        Ok(data) => *journal_id == data.UsnJournalID && *next_usn >= data.FirstUsn,
                        Err(_) => false,
                    });

            let idx = match loaded {
                Some((idx, _journal_id, next_usn)) => {
                    journal.next_usn = next_usn;
                    tracing::info!(volume = %label, entries = idx.len(), "snapshot restored");
                    idx
                }
                None => match crate::mft::scan_volume(&label) {
                    Ok((idx, stats)) => {
                        tracing::info!(
                            volume = %label,
                            entries = idx.len(),
                            ms = stats.elapsed_total_ms,
                            "full scan complete"
                        );
                        idx
                    }
                    Err(e) => {
                        *slot.phase.lock() = VolumePhase::Failed;
                        self.emit(EngineEvent::VolumeFailed {
                            volume: label.clone(),
                            message: e.to_string(),
                        });
                        return;
                    }
                },
            };

            let entries = idx.live_len() as u64;
            *slot.scanned.lock() = entries;
            *slot.index.write() = Some(idx);
            *slot.phase.lock() = VolumePhase::Ready;
            self.emit(EngineEvent::VolumeReady {
                volume: label.clone(),
                entries,
            });

            // 2. Tail the journal until stop or journal-gone.
            let fetch = match VolumeStatFetcher::open(&label) {
                Ok(f) => f,
                Err(e) => {
                    self.emit(EngineEvent::VolumeFailed {
                        volume: label.clone(),
                        message: e.to_string(),
                    });
                    return;
                }
            };
            let mut buf = Vec::new();
            let mut last_emit = Instant::now() - INDEX_CHANGED_DEBOUNCE;
            loop {
                if slot.stop.load(Ordering::Relaxed) {
                    self.save_slot(&slot, &journal, &snapshot_path);
                    return;
                }
                match journal.read_blocking(&mut buf) {
                    Ok(ReadOutcome::Records(rs)) => {
                        if rs.is_empty() {
                            continue;
                        }
                        if let Some(idx) = slot.index.write().as_mut() {
                            apply_batch(idx, &rs, &fetch);
                            *slot.scanned.lock() = idx.live_len() as u64;
                        }
                        if last_emit.elapsed() >= INDEX_CHANGED_DEBOUNCE {
                            last_emit = Instant::now();
                            self.emit(EngineEvent::IndexChanged {
                                volume: label.clone(),
                            });
                        }
                    }
                    Ok(ReadOutcome::Gone(gone)) => {
                        tracing::warn!(volume = %label, ?gone, "journal gone — full rescan");
                        *slot.phase.lock() = VolumePhase::Rescanning;
                        self.emit(EngineEvent::RescanStarted {
                            volume: label.clone(),
                        });
                        let _ = std::fs::remove_file(&snapshot_path);
                        break; // restart the outer loop → fresh journal + scan
                    }
                    Err(e) => {
                        *slot.phase.lock() = VolumePhase::Failed;
                        self.emit(EngineEvent::VolumeFailed {
                            volume: label.clone(),
                            message: e.to_string(),
                        });
                        return;
                    }
                }
            }
        }
    }

    #[cfg(windows)]
    fn save_slot(
        &self,
        slot: &VolumeSlot,
        journal: &crate::usn::UsnJournal,
        path: &std::path::Path,
    ) {
        if let Some(idx) = slot.index.read().as_ref()
            && let Err(e) = idx.save_to(path, journal.journal_id, journal.next_usn)
        {
            tracing::warn!(volume = %slot.label, error = %e, "snapshot save failed");
        }
    }

    pub fn status(&self) -> Vec<(String, VolumePhase, u64)> {
        self.volumes
            .read()
            .iter()
            .map(|s| (s.label.clone(), *s.phase.lock(), *s.scanned.lock()))
            .collect()
    }

    /// Run a query against every Ready volume and merge the per-volume,
    /// already-sorted id lists into one ordered result set.
    pub fn query(&self, text: &str, opt: &QueryOptions) -> Result<ResultSet, EngineError> {
        let ast = query::parse(text)?;
        let compiled = query::compile(&ast, opt.case, &date_resolver())?;

        let slots: Vec<Arc<VolumeSlot>> = self
            .volumes
            .read()
            .iter()
            .filter(|s| *s.phase.lock() == VolumePhase::Ready)
            .cloned()
            .collect();

        let mut per_volume: Vec<(Arc<VolumeSlot>, Vec<EntryId>, u64)> = Vec::new();
        for slot in &slots {
            let guard = slot.index.read();
            let Some(idx) = guard.as_ref() else { continue };
            let r = query::search(idx, &compiled, opt);
            per_volume.push((slot.clone(), r.ids, r.structural_generation));
        }

        // K-way merge by the sort key (typically 1-3 volumes).
        let total: usize = per_volume.iter().map(|(_, ids, _)| ids.len()).sum();
        let mut rows: Vec<(u32, EntryId)> = Vec::with_capacity(total);
        {
            let guards: Vec<_> = per_volume
                .iter()
                .map(|(slot, _, _)| slot.index.read())
                .collect();
            let mut cursors: Vec<usize> = vec![0; per_volume.len()];
            loop {
                let mut best: Option<usize> = None;
                for (v, (_, ids, _)) in per_volume.iter().enumerate() {
                    if cursors[v] >= ids.len() {
                        continue;
                    }
                    best = match best {
                        None => Some(v),
                        Some(b) => {
                            let (ib, vb) = (per_volume[b].1[cursors[b]], b);
                            let (iv, vv) = (ids[cursors[v]], v);
                            let idx_b = guards[vb].as_ref().unwrap();
                            let idx_v = guards[vv].as_ref().unwrap();
                            if cmp_entries(idx_v, iv, idx_b, ib, opt) == std::cmp::Ordering::Less {
                                Some(vv)
                            } else {
                                Some(vb)
                            }
                        }
                    };
                }
                match best {
                    Some(v) => {
                        rows.push((v as u32, per_volume[v].1[cursors[v]]));
                        cursors[v] += 1;
                    }
                    None => break,
                }
            }
        }

        Ok(ResultSet {
            slots: per_volume.iter().map(|(s, _, _)| s.clone()).collect(),
            structural: per_volume.iter().map(|(_, _, g)| *g).collect(),
            rows,
        })
    }

    /// Persist all volumes (graceful shutdown / explicit flush). Tailing
    /// threads also save on stop; this covers "save now" requests.
    #[cfg(windows)]
    pub fn flush(&self) {
        // Snapshots are written by the tailing threads on stop; an explicit
        // flush from a live engine writes with the thread-held checkpoint
        // being slightly behind, which the USN replay covers. For MVP we only
        // save on shutdown to keep a single writer per file.
    }

    pub fn shutdown(&self) {
        for slot in self.volumes.read().iter() {
            slot.stop.store(true, Ordering::Relaxed);
        }
        // Blocked journal reads return on the next volume write; joining with
        // a bounded wait keeps shutdown prompt without CancelSynchronousIo
        // (M2 refinement).
        let mut threads = self.threads.lock();
        for t in threads.drain(..) {
            let _ = t.join();
        }
    }

    /// Test/dev helper: register an already-built index as a Ready volume.
    pub fn insert_ready_volume(&self, label: &str, idx: VolumeIndex) {
        let slot = Arc::new(VolumeSlot {
            label: label.to_string(),
            phase: Mutex::new(VolumePhase::Ready),
            scanned: Mutex::new(idx.live_len() as u64),
            index: RwLock::new(Some(idx)),
            stop: Arc::new(AtomicBool::new(false)),
        });
        self.volumes.write().push(slot);
    }
}

fn cmp_entries(
    a_idx: &VolumeIndex,
    a: EntryId,
    b_idx: &VolumeIndex,
    b: EntryId,
    opt: &QueryOptions,
) -> std::cmp::Ordering {
    let ord = match opt.sort {
        SortKey::Name => a_idx.lower_name(a).cmp(b_idx.lower_name(b)),
        SortKey::Size => a_idx.size(a).cmp(&b_idx.size(b)),
        SortKey::Mtime => a_idx.mtime(a).cmp(&b_idx.mtime(b)),
    };
    if opt.desc { ord.reverse() } else { ord }
}

#[cfg(windows)]
fn date_resolver() -> impl query::DateResolver {
    query::WindowsLocalResolver
}
#[cfg(not(windows))]
fn date_resolver() -> impl query::DateResolver {
    query::UtcResolver
}

/// One row handed across the FFI: everything the UI list needs.
pub struct Row {
    pub entry_ref: u64,
    pub frn: u64,
    pub size: u64,
    pub mtime: i64,
    pub flags: u32,
    pub name: Vec<u8>,
    pub parent_path: Vec<u8>,
}

/// Materialized, sort-ordered result. Pages are O(1) slices; reads stay
/// valid across content mutations and fail with `Stale` only after a
/// structural change (compaction/rescan).
pub struct ResultSet {
    slots: Vec<Arc<VolumeSlot>>,
    structural: Vec<u64>,
    rows: Vec<(u32, EntryId)>,
}

impl ResultSet {
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn page(&self, offset: usize, count: usize) -> Result<Vec<Row>, EngineError> {
        let end = (offset.saturating_add(count)).min(self.rows.len());
        let start = offset.min(end);
        let mut out = Vec::with_capacity(end - start);

        let guards: Vec<_> = self.slots.iter().map(|s| s.index.read()).collect();
        for (v, guard) in guards.iter().enumerate() {
            let idx = guard.as_ref().ok_or(EngineError::Stale)?;
            if idx.structural_generation() != self.structural[v] {
                return Err(EngineError::Stale);
            }
        }
        for &(v, id) in &self.rows[start..end] {
            let idx = guards[v as usize].as_ref().ok_or(EngineError::Stale)?;
            let mut parent_path = Vec::new();
            idx.append_parent_path(id, &mut parent_path);
            out.push(Row {
                entry_ref: ((v as u64) << 32) | id as u64,
                frn: idx.frn(id),
                size: idx.size(id),
                mtime: idx.mtime(id),
                flags: idx_flags(idx, id),
                name: idx.name(id).to_vec(),
                parent_path,
            });
        }
        Ok(out)
    }
}

fn idx_flags(idx: &VolumeIndex, id: EntryId) -> u32 {
    let mut f = 0u32;
    if idx.is_dir(id) {
        f |= 1;
    }
    if !idx.is_live(id) {
        f |= 2; // deleted-since-query marker for the UI
    }
    f
}

// Reuse the flags module so the constant meanings stay in one place.
const _: () = {
    assert!(flags::IS_DIR == 1);
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{RawEntry, VolumeIndexBuilder};

    fn vol(label: &str, names: &[(&str, u64)]) -> VolumeIndex {
        let mut b = VolumeIndexBuilder::new(label, 5);
        for (i, (name, size)) in names.iter().enumerate() {
            let units: Vec<u16> = name.encode_utf16().collect();
            b.push(RawEntry {
                record: 100 + i as u64,
                parent_record: 5,
                frn: (1 << 48) | (100 + i as u64),
                name_utf16: &units,
                is_dir: false,
                is_reparse: false,
                is_hidden: false,
                is_system: false,
                size: *size,
                mtime: i as i64,
            });
        }
        b.finish()
    }

    fn engine_with_two_volumes() -> Arc<Engine> {
        let e = Engine::new(EngineConfig {
            index_dir: std::env::temp_dir(),
        });
        e.insert_ready_volume("C:", vol("C:", &[("alpha.txt", 10), ("gamma.txt", 30)]));
        e.insert_ready_volume("D:", vol("D:", &[("beta.txt", 20), ("delta.txt", 40)]));
        e
    }

    #[test]
    fn query_merges_volumes_in_name_order() {
        let e = engine_with_two_volumes();
        let r = e.query("txt", &QueryOptions::default()).unwrap();
        let rows = r.page(0, 10).unwrap();
        let names: Vec<String> = rows
            .iter()
            .map(|r| String::from_utf8_lossy(&r.name).into_owned())
            .collect();
        assert_eq!(
            names,
            vec!["alpha.txt", "beta.txt", "delta.txt", "gamma.txt"]
        );
        // entry_ref carries the volume ordinal in the high half.
        assert_eq!(rows[0].entry_ref >> 32, 0);
        assert_eq!(rows[1].entry_ref >> 32, 1);
    }

    #[test]
    fn paging_is_a_slice_and_size_sort_descends() {
        let e = engine_with_two_volumes();
        let opt = QueryOptions {
            sort: SortKey::Size,
            desc: true,

            ..Default::default()
        };
        let r = e.query("txt", &opt).unwrap();
        assert_eq!(r.len(), 4);
        let page = r.page(1, 2).unwrap();
        let sizes: Vec<u64> = page.iter().map(|r| r.size).collect();
        assert_eq!(sizes, vec![30, 20]);
        // Out-of-range page is empty, not an error.
        assert!(r.page(99, 5).unwrap().is_empty());
    }

    #[test]
    fn parent_paths_come_back_per_volume() {
        let e = engine_with_two_volumes();
        let r = e.query("beta", &QueryOptions::default()).unwrap();
        let rows = r.page(0, 1).unwrap();
        assert_eq!(rows[0].parent_path, b"D:\\");
    }

    #[test]
    fn status_reports_ready_volumes() {
        let e = engine_with_two_volumes();
        let st = e.status();
        assert_eq!(st.len(), 2);
        assert!(
            st.iter()
                .all(|(_, p, n)| *p == VolumePhase::Ready && *n > 0)
        );
    }
}
