// ===== File: meta.rs — partition.meta (M2, PLAN-M2 §1a): persisted hw/leader_epoch =====
//
// One small fixed-size sidecar file per partition directory, recording the
// two bits of replication state that must survive a restart:
// `high_watermark` (PLAN-M2 §0 K-M2-1: "hw jest monotoniczny i trwały per
// partycja" — a promoted leader's hw must never regress, which only holds
// across a crash if it was actually persisted, not just held in memory) and
// `leader_epoch` (so a former leader that rejoins after a restart cannot
// resume writing with a stale epoch — `Partition::set_leader_epoch`'s
// monotonic check is only as good as what a restart remembers). `leo_hint`
// is recorded for diagnostics only: `Partition::open`'s crash recovery
// always re-derives the real `log_end_offset` from the segment scan, the
// only source of truth for it — this field is never read back into that
// computation.
//
// Format (fixed 30 bytes, little-endian):
// `[u32 magic][u16 ver][u64 hw][u32 leader_epoch][u64 leo_hint][u32 crc32c]`.
// `crc32c` covers every byte before it. Written atomically: a temp file is
// written and fsynced, then renamed over the real path (POSIX rename is
// atomic within one filesystem), then the containing directory is fsynced
// so the rename itself survives a crash — the same discipline
// `segment::fsync_dir` uses for segment/roll durability (duplicated here
// rather than imported since that helper is private to `segment`).
//
// All writes to this file go through the partition's single writer thread
// (`partition::WriterCommand::PersistMeta`, plus direct calls from `roll()`
// and the writer's own shutdown/periodic paths) — never from an arbitrary
// caller thread — so two writers can never race the same tmp path.
//
// M1 compatibility (PLAN-M2 §4.1 A2): a partition directory written before
// this file existed has no `partition.meta` at all. `read_meta` treats a
// missing file as `None`, and `Partition::open` maps `None` to `hw = leo`
// — the only fallback that keeps every M1 partition's data visible after
// an upgrade instead of hiding it all behind a `hw` frozen at 0.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::error::{BusError, Result};

const MAGIC: u32 = 0x5442_4d31; // ASCII-ish "TBM1" — TentaBus partition Meta v1
const VERSION: u16 = 1;
const ENCODED_LEN: usize = 4 + 2 + 8 + 4 + 8 + 4; // 30 bytes

pub fn meta_path(dir: &Path) -> PathBuf {
    dir.join("partition.meta")
}

fn tmp_path(dir: &Path) -> PathBuf {
    dir.join("partition.meta.tmp")
}

/// The persisted contents of `partition.meta`. See this module's doc for
/// the wire format and every field's meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PartitionMeta {
    pub high_watermark: u64,
    pub leader_epoch: u32,
    pub leo_hint: u64,
}

impl PartitionMeta {
    fn encode(&self) -> [u8; ENCODED_LEN] {
        let mut buf = [0u8; ENCODED_LEN];
        buf[0..4].copy_from_slice(&MAGIC.to_le_bytes());
        buf[4..6].copy_from_slice(&VERSION.to_le_bytes());
        buf[6..14].copy_from_slice(&self.high_watermark.to_le_bytes());
        buf[14..18].copy_from_slice(&self.leader_epoch.to_le_bytes());
        buf[18..26].copy_from_slice(&self.leo_hint.to_le_bytes());
        let crc = crc32c::crc32c(&buf[..26]);
        buf[26..30].copy_from_slice(&crc.to_le_bytes());
        buf
    }

    /// Decodes `buf`, returning `None` for anything that is not a
    /// well-formed, checksum-valid v1 record: too short, wrong magic/
    /// version, or a CRC mismatch (torn write, disk corruption). The
    /// caller (`read_meta`) logs a warning in that case and falls back
    /// exactly as if the file did not exist at all.
    fn decode(buf: &[u8]) -> Option<Self> {
        if buf.len() < ENCODED_LEN {
            return None;
        }
        let buf = &buf[..ENCODED_LEN];
        let magic = u32::from_le_bytes(buf[0..4].try_into().unwrap());
        if magic != MAGIC {
            return None;
        }
        let ver = u16::from_le_bytes(buf[4..6].try_into().unwrap());
        if ver != VERSION {
            return None;
        }
        let crc = u32::from_le_bytes(buf[26..30].try_into().unwrap());
        if crc32c::crc32c(&buf[..26]) != crc {
            return None;
        }
        Some(PartitionMeta {
            high_watermark: u64::from_le_bytes(buf[6..14].try_into().unwrap()),
            leader_epoch: u32::from_le_bytes(buf[14..18].try_into().unwrap()),
            leo_hint: u64::from_le_bytes(buf[18..26].try_into().unwrap()),
        })
    }
}

/// Writes `meta` to `dir`'s `partition.meta`, atomically (tmp file, fsync,
/// rename, directory fsync). Overwrites whatever was there before.
pub fn write_meta(dir: &Path, meta: &PartitionMeta) -> Result<()> {
    let tmp = tmp_path(dir);
    let final_path = meta_path(dir);
    {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp)
            .map_err(|e| BusError::io(&tmp, e))?;
        f.write_all(&meta.encode())
            .map_err(|e| BusError::io(&tmp, e))?;
        f.sync_all().map_err(|e| BusError::io(&tmp, e))?;
    }
    std::fs::rename(&tmp, &final_path).map_err(|e| BusError::io(&final_path, e))?;
    // fsync the directory so the rename itself (the new/replaced directory
    // entry) is durable — `segment::fsync_dir`'s exact reasoning, applied
    // here to this sidecar file's rename instead of a segment roll.
    std::fs::File::open(dir)
        .and_then(|f| f.sync_all())
        .map_err(|e| BusError::io(dir, e))?;
    Ok(())
}

/// Reads `dir`'s `partition.meta`. Returns `None` for a missing file (the
/// expected, silent M1-compatibility case — see this module's doc) as well
/// as for a short/corrupt one (logged via `tracing::warn!` so an operator
/// can notice repeated corruption, but still treated as "start from leo"
/// rather than a hard error — a partition must stay openable even if its
/// metadata sidecar is damaged).
pub fn read_meta(dir: &Path) -> Option<PartitionMeta> {
    let path = meta_path(dir);
    match std::fs::read(&path) {
        Ok(bytes) => match PartitionMeta::decode(&bytes) {
            Some(m) => Some(m),
            None => {
                tracing::warn!(
                    path = %path.display(),
                    len = bytes.len(),
                    "partition.meta is short or corrupt (bad magic/version/crc); \
                     falling back to hw = leo as if the file did not exist"
                );
                None
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "failed to read partition.meta; falling back to hw = leo as if the file did not exist"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::temp_dir;

    #[test]
    fn write_then_read_round_trips() {
        let dir = temp_dir("meta-roundtrip");
        let meta = PartitionMeta {
            high_watermark: 42,
            leader_epoch: 7,
            leo_hint: 100,
        };
        write_meta(&dir, &meta).unwrap();
        assert_eq!(read_meta(&dir), Some(meta));
    }

    #[test]
    fn write_overwrites_a_previous_value_atomically() {
        let dir = temp_dir("meta-overwrite");
        write_meta(
            &dir,
            &PartitionMeta {
                high_watermark: 1,
                leader_epoch: 0,
                leo_hint: 1,
            },
        )
        .unwrap();
        write_meta(
            &dir,
            &PartitionMeta {
                high_watermark: 99,
                leader_epoch: 3,
                leo_hint: 99,
            },
        )
        .unwrap();
        let m = read_meta(&dir).unwrap();
        assert_eq!(m.high_watermark, 99);
        assert_eq!(m.leader_epoch, 3);
        // The tmp file must never linger after a successful rename.
        assert!(!tmp_path(&dir).exists());
    }

    /// A1/A2 (PLAN-M2 §4.1): a directory with no `partition.meta` at all —
    /// exactly what every M1 partition looks like — must read back as
    /// `None`, not an error.
    #[test]
    fn missing_file_reads_as_none() {
        let dir = temp_dir("meta-missing");
        assert_eq!(read_meta(&dir), None);
        assert!(!meta_path(&dir).exists());
    }

    #[test]
    fn short_file_is_treated_as_missing() {
        let dir = temp_dir("meta-short");
        std::fs::write(meta_path(&dir), [0u8; 10]).unwrap();
        assert_eq!(read_meta(&dir), None);
    }

    #[test]
    fn corrupt_crc_is_treated_as_missing() {
        let dir = temp_dir("meta-bad-crc");
        let meta = PartitionMeta {
            high_watermark: 5,
            leader_epoch: 1,
            leo_hint: 5,
        };
        let mut bytes = meta.encode().to_vec();
        // Flip a byte inside the payload without touching the trailing CRC
        // field itself, so the stored CRC no longer matches.
        bytes[10] ^= 0xFF;
        std::fs::write(meta_path(&dir), &bytes).unwrap();
        assert_eq!(read_meta(&dir), None);
    }

    #[test]
    fn bad_magic_is_treated_as_missing() {
        let dir = temp_dir("meta-bad-magic");
        let mut bytes = PartitionMeta {
            high_watermark: 1,
            leader_epoch: 1,
            leo_hint: 1,
        }
        .encode()
        .to_vec();
        bytes[0..4].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
        std::fs::write(meta_path(&dir), &bytes).unwrap();
        assert_eq!(read_meta(&dir), None);
    }
}
