// ===== File: epochs.rs — partition.epochs: leader epoch -> first offset written in it =====
//
// Kafka's leader-epoch checkpoint, one per partition directory. Every record
// carries the epoch of the leader that first wrote it — a leader stamps its
// own term on what it appends, a follower keeps whatever epoch the leader
// says the record was written in — and this table records, for each epoch,
// the offset of its first record. Epochs only grow along a log, so the table
// is sorted by both columns and `epoch_at(offset)` is a binary search.
//
// Replication ranks logs by the epoch of their LAST record
// (`last_epoch`, Raft's "last log term"). That must describe the records
// on disk, never the term of a leader the partition merely accepted: a
// leader re-elected in a new term feeds a follower records of the old one,
// and those records keep their old epoch here.
//
// Crash safety is the ordering at the two call sites, not this file:
// `Partition` persists an entry for a new epoch BEFORE the first record of
// that epoch is written (a crash leaves the table claiming at most the one
// record that was about to land), and trims the table BEFORE a truncate cuts
// records (a crash leaves the table describing fewer records than the log —
// an older, never a newer, last epoch). `Partition::open` trims whatever
// lies past the recovered log end.
//
// Format, little-endian:
// `[u32 magic][u16 ver][u32 count]{count × [u32 epoch][u64 start_offset]}[u32 crc32c]`.
// Written like `partition.meta`: temp file, fsync, rename, directory fsync.
// A missing or corrupt file reads as an empty table: every record then has
// epoch 0, which ranks this log below every term and makes the next leader
// reconcile it to its committed offset — the conservative reading.
//
// CLAIMS. An entry may start exactly at the log end, with no record behind
// it yet: "this log is a prefix of that epoch's leader chain, and that
// chain's records from here on are all of that epoch or later". It is Raft's
// no-op entry without an offset. A leader stamps one for its own term when it
// starts serving and a follower copies it once its log reaches that point
// (`Partition::confirm_epoch`), so a majority can rank above a stale
// later-term log without the leader writing a record consumers would see.
// The same shape is what an append that crashed between the entry and its
// record leaves, and it is true there too: the record was that epoch
// leader's, at that offset, over this very prefix. So a claim survives a
// restart; only a cut below it (`truncated_to`) or the first record at its
// offset (`with_record`) replaces it.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::error::{BusError, Result};

const MAGIC: u32 = 0x5442_4531; // "TBE1"
const VERSION: u16 = 1;
const HEADER_LEN: usize = 4 + 2 + 4;
const ENTRY_LEN: usize = 4 + 8;

pub fn epochs_path(dir: &Path) -> PathBuf {
    dir.join("partition.epochs")
}

fn tmp_path(dir: &Path) -> PathBuf {
    dir.join("partition.epochs.tmp")
}

/// `(epoch, first offset written in it)`, ascending in both.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EpochTable {
    entries: Vec<(u32, u64)>,
}

impl EpochTable {
    pub fn entries(&self) -> &[(u32, u64)] {
        &self.entries
    }

    /// The epoch of the record at `offset`: the latest entry starting at or
    /// before it, `0` before the first entry.
    pub fn epoch_at(&self, offset: u64) -> u32 {
        let idx = self.entries.partition_point(|&(_, start)| start <= offset);
        if idx == 0 {
            0
        } else {
            self.entries[idx - 1].0
        }
    }

    /// The epoch of the log's last record; `0` for an empty table.
    pub fn last_epoch(&self) -> u32 {
        self.entries.last().map(|&(epoch, _)| epoch).unwrap_or(0)
    }

    /// The claim at `log_end` (an entry starting there), if the log ends in
    /// one.
    pub fn claim_at(&self, log_end: u64) -> Option<(u32, u64)> {
        self.entries
            .last()
            .copied()
            .filter(|&(_, start)| start == log_end)
    }

    /// The offset of the first record written in `epoch` — or of its claim,
    /// when the log ends in one — if any.
    pub fn start_of(&self, epoch: u32) -> Option<u64> {
        self.entries
            .iter()
            .find(|&&(e, _)| e == epoch)
            .map(|&(_, start)| start)
    }

    /// The table after a record of `epoch` lands at `offset` (the log
    /// end), or `None` when the table does not change (same epoch as the
    /// last entry). An epoch below the last record's is refused: records of
    /// an older leader after records of a newer one mean this log is not a
    /// prefix of any leader's chain. A claim at `offset` holds no record, so
    /// the record written there replaces it whatever its epoch: the leader
    /// sending it is the authority on what its chain holds at that offset.
    pub fn with_record(&self, epoch: u32, offset: u64) -> Result<Option<Self>> {
        if !self.entries.is_empty() && epoch == self.last_epoch() {
            return Ok(None);
        }
        let mut next = self.clone();
        while next
            .entries
            .last()
            .is_some_and(|&(_, start)| start >= offset)
        {
            next.entries.pop();
        }
        let last = next.last_epoch();
        if !next.entries.is_empty() && epoch < last {
            return Err(BusError::RecordEpochRegression {
                last,
                got: epoch,
                offset,
            });
        }
        next.entries.push((epoch, offset));
        Ok(Some(next))
    }

    /// The table with a claim for `epoch` at `log_end` (see the module
    /// doc), or `None` when the log already names `epoch` or a later one.
    pub fn with_claim(&self, epoch: u32, log_end: u64) -> Option<Self> {
        if !self.entries.is_empty() && epoch <= self.last_epoch() {
            return None;
        }
        let mut next = self.clone();
        while next
            .entries
            .last()
            .is_some_and(|&(_, start)| start >= log_end)
        {
            next.entries.pop();
        }
        next.entries.push((epoch, log_end));
        Some(next)
    }

    /// The table of a log recovered with `log_end` records: entries past
    /// it named records that are gone, while one starting exactly there is a
    /// claim and stays. `None` when nothing changes.
    pub fn recovered_to(&self, log_end: u64) -> Option<Self> {
        let keep = self.entries.partition_point(|&(_, start)| start <= log_end);
        if keep == self.entries.len() {
            return None;
        }
        Some(Self {
            entries: self.entries[..keep].to_vec(),
        })
    }

    /// The table describing only records below `log_end`, claims at it
    /// included in the cut, or `None` when nothing changes.
    pub fn truncated_to(&self, log_end: u64) -> Option<Self> {
        let keep = self.entries.partition_point(|&(_, start)| start < log_end);
        if keep == self.entries.len() {
            return None;
        }
        Some(Self {
            entries: self.entries[..keep].to_vec(),
        })
    }

    fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(HEADER_LEN + self.entries.len() * ENTRY_LEN + 4);
        buf.extend_from_slice(&MAGIC.to_le_bytes());
        buf.extend_from_slice(&VERSION.to_le_bytes());
        buf.extend_from_slice(&(self.entries.len() as u32).to_le_bytes());
        for &(epoch, start) in &self.entries {
            buf.extend_from_slice(&epoch.to_le_bytes());
            buf.extend_from_slice(&start.to_le_bytes());
        }
        let crc = crc32c::crc32c(&buf);
        buf.extend_from_slice(&crc.to_le_bytes());
        buf
    }

    fn decode(buf: &[u8]) -> Option<Self> {
        if buf.len() < HEADER_LEN + 4 {
            return None;
        }
        if u32::from_le_bytes(buf[0..4].try_into().unwrap()) != MAGIC
            || u16::from_le_bytes(buf[4..6].try_into().unwrap()) != VERSION
        {
            return None;
        }
        let count = u32::from_le_bytes(buf[6..10].try_into().unwrap()) as usize;
        let body_end = HEADER_LEN.checked_add(count.checked_mul(ENTRY_LEN)?)?;
        if buf.len() < body_end + 4 {
            return None;
        }
        let crc = u32::from_le_bytes(buf[body_end..body_end + 4].try_into().unwrap());
        if crc32c::crc32c(&buf[..body_end]) != crc {
            return None;
        }
        let mut entries = Vec::with_capacity(count);
        for i in 0..count {
            let at = HEADER_LEN + i * ENTRY_LEN;
            let epoch = u32::from_le_bytes(buf[at..at + 4].try_into().unwrap());
            let start = u64::from_le_bytes(buf[at + 4..at + 12].try_into().unwrap());
            if let Some(&(prev_epoch, prev_start)) = entries.last() {
                if epoch <= prev_epoch || start < prev_start {
                    return None;
                }
            }
            entries.push((epoch, start));
        }
        Some(Self { entries })
    }
}

/// Writes `table` to `dir`'s `partition.epochs` atomically.
pub fn write_epochs(dir: &Path, table: &EpochTable) -> Result<()> {
    let tmp = tmp_path(dir);
    let final_path = epochs_path(dir);
    {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp)
            .map_err(|e| BusError::io(&tmp, e))?;
        f.write_all(&table.encode())
            .map_err(|e| BusError::io(&tmp, e))?;
        f.sync_all().map_err(|e| BusError::io(&tmp, e))?;
    }
    std::fs::rename(&tmp, &final_path).map_err(|e| BusError::io(&final_path, e))?;
    std::fs::File::open(dir)
        .and_then(|f| f.sync_all())
        .map_err(|e| BusError::io(dir, e))?;
    Ok(())
}

/// Reads `dir`'s `partition.epochs`; a missing or corrupt file is an empty
/// table (see the module doc for why that is the conservative reading).
pub fn read_epochs(dir: &Path) -> EpochTable {
    let path = epochs_path(dir);
    match std::fs::read(&path) {
        Ok(bytes) => EpochTable::decode(&bytes).unwrap_or_else(|| {
            tracing::warn!(
                path = %path.display(),
                "partition.epochs is corrupt; every record reads as epoch 0 until rewritten"
            );
            EpochTable::default()
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => EpochTable::default(),
        Err(e) => {
            tracing::warn!(
                path = %path.display(), error = %e,
                "failed to read partition.epochs; every record reads as epoch 0 until rewritten"
            );
            EpochTable::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::temp_dir;

    fn table(entries: &[(u32, u64)]) -> EpochTable {
        EpochTable {
            entries: entries.to_vec(),
        }
    }

    #[test]
    fn epoch_at_finds_the_epoch_a_record_was_written_in() {
        let t = table(&[(1, 0), (3, 10), (4, 20)]);
        assert_eq!(t.epoch_at(0), 1);
        assert_eq!(t.epoch_at(9), 1);
        assert_eq!(t.epoch_at(10), 3);
        assert_eq!(t.epoch_at(25), 4);
        assert_eq!(table(&[(2, 5)]).epoch_at(4), 0);
        assert_eq!(t.last_epoch(), 4);
        assert_eq!(t.start_of(3), Some(10));
        assert_eq!(t.start_of(2), None);
    }

    #[test]
    fn a_record_of_an_older_epoch_after_a_newer_one_is_refused() {
        let t = table(&[(3, 0)]);
        assert_eq!(t.with_record(3, 5).unwrap(), None);
        assert_eq!(t.with_record(4, 5).unwrap(), Some(table(&[(3, 0), (4, 5)])));
        assert!(matches!(
            t.with_record(2, 5),
            Err(BusError::RecordEpochRegression {
                last: 3,
                got: 2,
                offset: 5
            })
        ));
    }

    #[test]
    fn a_record_replaces_a_claim_at_its_offset_whatever_its_epoch() {
        let claimed = table(&[(3, 0), (5, 4)]);
        assert_eq!(claimed.with_record(5, 4).unwrap(), None, "fills the claim");
        assert_eq!(
            claimed.with_record(4, 4).unwrap(),
            Some(table(&[(3, 0), (4, 4)])),
            "the serving leader's chain holds an epoch-4 record there"
        );
        assert_eq!(
            claimed.with_record(6, 4).unwrap(),
            Some(table(&[(3, 0), (6, 4)]))
        );
        assert!(matches!(
            claimed.with_record(2, 4),
            Err(BusError::RecordEpochRegression {
                last: 3,
                got: 2,
                ..
            })
        ));
    }

    #[test]
    fn a_claim_needs_a_later_epoch_and_survives_recovery_but_not_a_cut() {
        let t = table(&[(3, 0)]);
        assert_eq!(t.with_claim(3, 4), None);
        assert_eq!(t.with_claim(2, 4), None);
        let claimed = t.with_claim(5, 4).unwrap();
        assert_eq!(claimed, table(&[(3, 0), (5, 4)]));
        assert_eq!(claimed.last_epoch(), 5);
        assert_eq!(claimed.epoch_at(3), 3, "no record changes its epoch");
        assert_eq!(claimed.with_claim(6, 4), Some(table(&[(3, 0), (6, 4)])));

        assert_eq!(claimed.recovered_to(4), None);
        assert_eq!(claimed.recovered_to(3), Some(t.clone()));
        assert_eq!(claimed.truncated_to(4), Some(t));
    }

    #[test]
    fn truncation_drops_the_epochs_that_start_at_or_past_the_new_end() {
        let t = table(&[(1, 0), (3, 10), (4, 20)]);
        assert_eq!(t.truncated_to(25), None);
        assert_eq!(t.truncated_to(20), Some(table(&[(1, 0), (3, 10)])));
        assert_eq!(t.truncated_to(0), Some(table(&[])));
    }

    #[test]
    fn the_table_round_trips_and_a_corrupt_file_reads_as_empty() {
        let dir = temp_dir("epochs-roundtrip");
        assert_eq!(read_epochs(&dir), EpochTable::default());
        let t = table(&[(1, 0), (3, 10)]);
        write_epochs(&dir, &t).unwrap();
        assert_eq!(read_epochs(&dir), t);
        let mut bytes = std::fs::read(epochs_path(&dir)).unwrap();
        bytes[12] ^= 0xFF;
        std::fs::write(epochs_path(&dir), bytes).unwrap();
        assert_eq!(read_epochs(&dir), EpochTable::default());
    }
}
