// ===== File: index.rs — sparse per-batch offset/time indexes (.oidx/.tidx) =====
//
// PLAN.md §2.3: one index entry per *batch*, not per record — at 1 MiB
// batches that is 8 KB of index per 1 GiB of log data, small enough to load
// entirely into RAM (PLAN: "indeksy są małe ... mieszczą się w RAM — seek to
// binsearch w pamięci + jeden pread"). Both index files are flat arrays of
// fixed-size little-endian records, appended in lockstep with the segment
// they describe and truncated in lockstep during crash recovery.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::RwLock;

use crate::error::{BusError, Result};

/// Entries live behind an `Arc<RwLock<..>>` (not a plain `Vec`) so a
/// `PartitionReader` can hold the same handle the writer thread appends to
/// and binary-search it under a read lock — no per-batch clone of a
/// potentially tens-of-thousands-of-entries array on the append hot path.
pub type SharedEntries<T> = Arc<RwLock<Vec<T>>>;

/// `.oidx` entry: batch's starting offset-delta (relative to the segment's
/// `base_offset`) and its byte position within the `.log` file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OffsetEntry {
    pub offset_delta: u32,
    pub file_pos: u32,
}

impl OffsetEntry {
    pub const ENCODED_LEN: usize = 8;

    fn encode(&self) -> [u8; Self::ENCODED_LEN] {
        let mut buf = [0u8; Self::ENCODED_LEN];
        buf[0..4].copy_from_slice(&self.offset_delta.to_le_bytes());
        buf[4..8].copy_from_slice(&self.file_pos.to_le_bytes());
        buf
    }

    fn decode(buf: &[u8]) -> Self {
        OffsetEntry {
            offset_delta: u32::from_le_bytes(buf[0..4].try_into().unwrap()),
            file_pos: u32::from_le_bytes(buf[4..8].try_into().unwrap()),
        }
    }
}

/// `.tidx` entry: batch's base timestamp and its offset-delta, so a
/// time-based seek resolves to the same coordinate space as `.oidx`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeEntry {
    pub ts_ms: i64,
    pub offset_delta: u32,
}

impl TimeEntry {
    pub const ENCODED_LEN: usize = 12;

    fn encode(&self) -> [u8; Self::ENCODED_LEN] {
        let mut buf = [0u8; Self::ENCODED_LEN];
        buf[0..8].copy_from_slice(&self.ts_ms.to_le_bytes());
        buf[8..12].copy_from_slice(&self.offset_delta.to_le_bytes());
        buf
    }

    fn decode(buf: &[u8]) -> Self {
        TimeEntry {
            ts_ms: i64::from_le_bytes(buf[0..8].try_into().unwrap()),
            offset_delta: u32::from_le_bytes(buf[8..12].try_into().unwrap()),
        }
    }
}

/// Loads `path` as a flat array of `entry_len`-byte records. Used by both
/// index flavors at open time; a partial trailing entry (torn write) is
/// dropped rather than treated as corruption, since the index is only a
/// hint — `Segment` recovery is the source of truth for the log itself.
fn load_entries(path: &Path, entry_len: usize) -> Result<Vec<u8>> {
    match std::fs::read(path) {
        Ok(mut bytes) => {
            let whole = (bytes.len() / entry_len) * entry_len;
            bytes.truncate(whole);
            Ok(bytes)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(BusError::io(path, e)),
    }
}

fn open_append(path: &Path) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| BusError::io(path, e))
}

/// Binary search for the last entry whose key is `<= target`, i.e. the
/// closest indexed position at or before the requested offset/time. Shared
/// by both index types via a small key-extraction closure to avoid
/// duplicating the search for two structurally different entry types.
fn floor_by<T: Copy, K: Ord>(entries: &[T], target: K, key: impl Fn(&T) -> K) -> Option<T> {
    if entries.is_empty() {
        return None;
    }
    match entries.binary_search_by(|e| key(e).cmp(&target)) {
        Ok(i) => Some(entries[i]),
        Err(0) => None, // target is before the first entry
        Err(i) => Some(entries[i - 1]),
    }
}

pub struct OffsetIndex {
    path: PathBuf,
    file: File,
    entries: SharedEntries<OffsetEntry>,
}

impl OffsetIndex {
    pub fn open_or_create(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let raw = load_entries(&path, OffsetEntry::ENCODED_LEN)?;
        let entries = raw
            .as_chunks::<{ OffsetEntry::ENCODED_LEN }>()
            .0
            .iter()
            .map(|c| OffsetEntry::decode(c))
            .collect();
        let file = open_append(&path)?;
        Ok(Self {
            path,
            file,
            entries: Arc::new(RwLock::new(entries)),
        })
    }

    pub fn append(&mut self, entry: OffsetEntry) -> Result<()> {
        self.file
            .write_all(&entry.encode())
            .map_err(|e| BusError::io(&self.path, e))?;
        self.entries.write().push(entry);
        Ok(())
    }

    /// Drops all entries and truncates the backing file to empty. Used when
    /// recovering the active segment: M0 re-scans that segment's log file
    /// from byte 0 (bounded by `max_segment_bytes`/`max_batches`, so this is
    /// cheap and not a hot path), so the simplest correct move is to throw
    /// the old index away and re-derive it entirely from the scan rather
    /// than reconcile a partially-stale index against fresh data.
    pub fn reset(&mut self) -> Result<()> {
        self.entries.write().clear();
        self.file
            .set_len(0)
            .map_err(|e| BusError::io(&self.path, e))?;
        self.file = open_append(&self.path)?;
        Ok(())
    }

    pub fn floor(&self, target_offset_delta: u32) -> Option<OffsetEntry> {
        floor_by(&self.entries.read(), target_offset_delta, |e| {
            e.offset_delta
        })
    }

    /// Drops every entry at or past `new_len` bytes and truncates the
    /// backing file to match what remains (M2, PLAN-M2 §1a:
    /// `Partition::truncate_to_offset`). Entries are appended in
    /// increasing `file_pos` order, so "keep entries with `file_pos <
    /// new_len`" is always a prefix of the array — the same
    /// rebuild-in-lockstep discipline `reset()` uses for full-segment
    /// recovery, but for a partial cut back to an earlier valid boundary
    /// instead of to empty.
    pub fn truncate_to_file_pos(&mut self, new_len: u32) -> Result<()> {
        let keep = {
            let mut entries = self.entries.write();
            entries.retain(|e| e.file_pos < new_len);
            entries.len()
        };
        self.file
            .set_len((keep * OffsetEntry::ENCODED_LEN) as u64)
            .map_err(|e| BusError::io(&self.path, e))?;
        self.file = open_append(&self.path)?;
        Ok(())
    }

    pub fn entries(&self) -> Vec<OffsetEntry> {
        self.entries.read().clone()
    }

    pub fn last(&self) -> Option<OffsetEntry> {
        self.entries.read().last().copied()
    }

    /// Clone of the shared handle, for a `PartitionReader` to binary-search
    /// under its own read lock without going through this `OffsetIndex`
    /// (which lives inside the writer thread's state).
    pub fn shared(&self) -> SharedEntries<OffsetEntry> {
        Arc::clone(&self.entries)
    }
}

pub struct TimeIndex {
    path: PathBuf,
    file: File,
    entries: SharedEntries<TimeEntry>,
}

impl TimeIndex {
    pub fn open_or_create(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let raw = load_entries(&path, TimeEntry::ENCODED_LEN)?;
        let entries = raw
            .as_chunks::<{ TimeEntry::ENCODED_LEN }>()
            .0
            .iter()
            .map(|c| TimeEntry::decode(c))
            .collect();
        let file = open_append(&path)?;
        Ok(Self {
            path,
            file,
            entries: Arc::new(RwLock::new(entries)),
        })
    }

    /// Appends `entry`, *unless* its timestamp does not strictly increase
    /// over the last appended entry, in which case the entry is silently
    /// dropped. `binary_search_by` in `floor_by` assumes a sorted key
    /// sequence; nothing enforces `base_timestamp_ms`
    /// monotonicity upstream (it comes straight from a producer-supplied
    /// `RecordInput::timestamp_ms`, PLAN §2.3), so with multiple producers
    /// or clock drift the raw sequence of batch timestamps is not
    /// guaranteed sorted. The time index is a sparse, best-effort seek
    /// hint (`fetch_from_timestamp` always falls back to a forward scan
    /// from the floor entry), so dropping an out-of-order entry costs
    /// nothing but a slightly coarser seek — accepting it would corrupt
    /// every subsequent binary search on this index.
    pub fn append(&mut self, entry: TimeEntry) -> Result<()> {
        if let Some(last) = self.entries.read().last() {
            if entry.ts_ms <= last.ts_ms {
                return Ok(());
            }
        }
        self.file
            .write_all(&entry.encode())
            .map_err(|e| BusError::io(&self.path, e))?;
        self.entries.write().push(entry);
        Ok(())
    }

    /// See `OffsetIndex::reset` — same rebuild-from-scratch recovery strategy.
    pub fn reset(&mut self) -> Result<()> {
        self.entries.write().clear();
        self.file
            .set_len(0)
            .map_err(|e| BusError::io(&self.path, e))?;
        self.file = open_append(&self.path)?;
        Ok(())
    }

    pub fn floor(&self, target_ts_ms: i64) -> Option<TimeEntry> {
        floor_by(&self.entries.read(), target_ts_ms, |e| e.ts_ms)
    }

    /// Drops every entry at or past `max_offset_delta` and truncates the
    /// backing file to match (M2, PLAN-M2 §1a:
    /// `Partition::truncate_to_offset`). Filtered by `offset_delta` rather
    /// than by position/count the way `OffsetIndex::truncate_to_file_pos`
    /// is: `TimeIndex::append` silently drops non-increasing timestamps, so
    /// this index can have fewer entries than the offset index for the same
    /// segment and the two cannot be kept in lockstep by count alone.
    pub fn truncate_to_offset_delta(&mut self, max_offset_delta: u32) -> Result<()> {
        let keep = {
            let mut entries = self.entries.write();
            entries.retain(|e| e.offset_delta < max_offset_delta);
            entries.len()
        };
        self.file
            .set_len((keep * TimeEntry::ENCODED_LEN) as u64)
            .map_err(|e| BusError::io(&self.path, e))?;
        self.file = open_append(&self.path)?;
        Ok(())
    }

    pub fn entries(&self) -> Vec<TimeEntry> {
        self.entries.read().clone()
    }

    pub fn shared(&self) -> SharedEntries<TimeEntry> {
        Arc::clone(&self.entries)
    }
}

/// Binary search a locked/borrowed slice for the last entry at or before
/// `target_offset_delta` — the exact lookup a `PartitionReader` performs
/// while holding an `OffsetIndex::shared()` read guard.
pub fn floor_offset(entries: &[OffsetEntry], target_offset_delta: u32) -> Option<OffsetEntry> {
    floor_by(entries, target_offset_delta, |e| e.offset_delta)
}

/// Same as `floor_offset` but for the time index.
pub fn floor_time(entries: &[TimeEntry], target_ts_ms: i64) -> Option<TimeEntry> {
    floor_by(entries, target_ts_ms, |e| e.ts_ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::temp_dir;

    #[test]
    fn offset_index_append_and_reload() {
        let dir = temp_dir("oidx-reload");
        let path = dir.join("00000000000000000000.oidx");
        {
            let mut idx = OffsetIndex::open_or_create(&path).unwrap();
            for i in 0..10u32 {
                idx.append(OffsetEntry {
                    offset_delta: i * 5,
                    file_pos: i * 1000,
                })
                .unwrap();
            }
        }
        let idx = OffsetIndex::open_or_create(&path).unwrap();
        assert_eq!(idx.entries().len(), 10);
        assert_eq!(
            idx.last(),
            Some(OffsetEntry {
                offset_delta: 45,
                file_pos: 9000
            })
        );
    }

    #[test]
    fn offset_index_floor_search() {
        let dir = temp_dir("oidx-floor");
        let path = dir.join("idx.oidx");
        let mut idx = OffsetIndex::open_or_create(&path).unwrap();
        for i in 0..5u32 {
            idx.append(OffsetEntry {
                offset_delta: i * 10,
                file_pos: i * 100,
            })
            .unwrap();
        }
        // entries at deltas 0,10,20,30,40
        assert_eq!(idx.floor(0).unwrap().offset_delta, 0);
        assert_eq!(idx.floor(5).unwrap().offset_delta, 0);
        assert_eq!(idx.floor(10).unwrap().offset_delta, 10);
        assert_eq!(idx.floor(39).unwrap().offset_delta, 30);
        assert_eq!(idx.floor(1000).unwrap().offset_delta, 40);
        // empty index: nothing at or before any target
        let empty = OffsetIndex::open_or_create(dir.join("empty.oidx")).unwrap();
        assert!(empty.floor(0).is_none());
    }

    #[test]
    fn offset_index_truncate_to_file_pos_keeps_only_the_prefix() {
        let dir = temp_dir("oidx-truncate");
        let path = dir.join("idx.oidx");
        let mut idx = OffsetIndex::open_or_create(&path).unwrap();
        for i in 0..5u32 {
            idx.append(OffsetEntry {
                offset_delta: i,
                file_pos: i * 100,
            })
            .unwrap();
        }
        // Keep entries with file_pos < 300: deltas 0,1,2 (pos 0,100,200).
        idx.truncate_to_file_pos(300).unwrap();
        assert_eq!(
            idx.entries(),
            vec![
                OffsetEntry {
                    offset_delta: 0,
                    file_pos: 0
                },
                OffsetEntry {
                    offset_delta: 1,
                    file_pos: 100
                },
                OffsetEntry {
                    offset_delta: 2,
                    file_pos: 200
                },
            ]
        );

        // Reload from disk to prove the file itself was truncated.
        let reloaded = OffsetIndex::open_or_create(&path).unwrap();
        assert_eq!(reloaded.entries().len(), 3);

        // Appending after a truncate resumes correctly.
        let mut idx = reloaded;
        idx.append(OffsetEntry {
            offset_delta: 10,
            file_pos: 300,
        })
        .unwrap();
        assert_eq!(idx.entries().len(), 4);
    }

    #[test]
    fn offset_index_reset() {
        let dir = temp_dir("oidx-reset");
        let path = dir.join("idx.oidx");
        let mut idx = OffsetIndex::open_or_create(&path).unwrap();
        for i in 0..5u32 {
            idx.append(OffsetEntry {
                offset_delta: i,
                file_pos: i * 100,
            })
            .unwrap();
        }
        idx.reset().unwrap();
        assert!(idx.entries().is_empty());

        // Reload from disk to prove the file itself was truncated, not just
        // the in-memory Vec.
        let reloaded = OffsetIndex::open_or_create(&path).unwrap();
        assert!(reloaded.entries().is_empty());
    }

    #[test]
    fn time_index_floor_search_and_reload() {
        let dir = temp_dir("tidx-basic");
        let path = dir.join("idx.tidx");
        {
            let mut idx = TimeIndex::open_or_create(&path).unwrap();
            for i in 0..5i64 {
                idx.append(TimeEntry {
                    ts_ms: 1_000 + i * 100,
                    offset_delta: i as u32,
                })
                .unwrap();
            }
        }
        let idx = TimeIndex::open_or_create(&path).unwrap();
        assert_eq!(idx.entries().len(), 5);
        assert!(idx.floor(999).is_none());
        assert_eq!(idx.floor(1_000).unwrap().offset_delta, 0);
        assert_eq!(idx.floor(1_250).unwrap().offset_delta, 2);
        assert_eq!(idx.floor(999_999).unwrap().offset_delta, 4);
    }

    /// A non-increasing timestamp (clock drift between producers, or a
    /// batch that is simply out of order) must be dropped rather than
    /// appended, since `floor` relies on `binary_search_by` over a sorted
    /// sequence.
    #[test]
    fn time_index_drops_non_increasing_timestamps() {
        let dir = temp_dir("tidx-monotonic");
        let path = dir.join("idx.tidx");
        let mut idx = TimeIndex::open_or_create(&path).unwrap();
        idx.append(TimeEntry {
            ts_ms: 1_000,
            offset_delta: 0,
        })
        .unwrap();
        idx.append(TimeEntry {
            ts_ms: 1_000, // equal, not strictly increasing -> dropped
            offset_delta: 1,
        })
        .unwrap();
        idx.append(TimeEntry {
            ts_ms: 900, // clock drift, earlier than the last entry -> dropped
            offset_delta: 2,
        })
        .unwrap();
        idx.append(TimeEntry {
            ts_ms: 1_500,
            offset_delta: 3,
        })
        .unwrap();

        let entries = idx.entries();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].ts_ms, 1_000);
        assert_eq!(entries[1].ts_ms, 1_500);
        assert_eq!(entries[1].offset_delta, 3);

        // Reload from disk: the dropped entries must never have been
        // written, not just filtered in memory.
        let reloaded = TimeIndex::open_or_create(&path).unwrap();
        assert_eq!(reloaded.entries().len(), 2);
    }

    #[test]
    fn time_index_truncate_to_offset_delta_keeps_only_the_prefix() {
        let dir = temp_dir("tidx-truncate");
        let path = dir.join("idx.tidx");
        let mut idx = TimeIndex::open_or_create(&path).unwrap();
        for i in 0..5i64 {
            idx.append(TimeEntry {
                ts_ms: 1_000 + i * 100,
                offset_delta: i as u32,
            })
            .unwrap();
        }
        // Keep entries with offset_delta < 3: deltas 0,1,2.
        idx.truncate_to_offset_delta(3).unwrap();
        let entries = idx.entries();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[2].offset_delta, 2);

        let reloaded = TimeIndex::open_or_create(&path).unwrap();
        assert_eq!(reloaded.entries().len(), 3);
    }
}
